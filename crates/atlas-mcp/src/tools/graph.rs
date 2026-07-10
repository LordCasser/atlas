//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use std::collections::HashSet;

use atlas_engine::dossier::SourceRepository;
use atlas_engine::symbol_selector::{MatchInfo, MatchMode};
use atlas_engine::{
    EdgeKind, InvestigationFocus, ScopedSearchRequest, ScopedSearchService, SearchAnalysis, Store,
    SymbolDef, SymbolId, SymbolKind, TraversalDirection,
};

use super::analysis_envelope::AnalysisEnvelope;
use super::{MAX_AMBIGUOUS_CANDIDATES, ToolRouter, get_str_opt, get_u64};
use crate::tools::symbol_selector::{
    ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy, SymbolSelector,
    parse_symbol_input,
};

use serde_json::json;

mod calls;
mod path;
mod explore;
mod impact;

// Re-export calls-dispatch items moved from mod.rs so they remain reachable
// via `tools::CallsDispatch` / `tools::resolve_calls_dispatch` (used by resume.rs
// and graph_tests.rs).  The `calls` module is private, so this bridge is required.
pub(crate) use calls::{CallsDispatch, resolve_calls_dispatch};

/// Extract the qualified name from a SymbolInput for display/logging.
fn symbol_input_qname(input: &SymbolInput) -> &str {
    match input {
        SymbolInput::Name(name) => name,
        SymbolInput::Selector(sel) => &sel.qualified_name,
    }
}

fn not_found_resolution_qname(resolution: &SymbolResolution) -> Option<&str> {
    match resolution {
        SymbolResolution::NotFound { qname, .. } => Some(qname),
        _ => None,
    }
}

/// Parse the "symbol" key from args as a SymbolInput.
/// Returns error if missing, null, or invalid.
fn parse_symbol_arg(args: &serde_json::Value) -> Result<SymbolInput, String> {
    parse_symbol_input(args, "symbol")
}

/// Parse a named field from args as a SymbolInput (e.g. "from" or "to").
fn parse_symbol_field(args: &serde_json::Value, field: &str) -> Result<SymbolInput, String> {
    parse_symbol_input(args, field)
}

/// Build the resolution metadata JSON object for Aggregate-policy responses.
fn build_resolution_meta(candidates: &[ScoredCandidate], count: usize) -> serde_json::Value {
    let matched: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| {
            json!({
                "qualified_name": c.qualified_name,
                "file_path": c.file_path,
                "line": c.line,
                "kind": c.kind,
                "language": c.language,
            })
        })
        .collect();
    json!({
        "policy": "aggregated",
        "count": count,
        "matched_candidates": matched,
    })
}

/// Convert a SymbolResolution (Aggregate policy) to (Vec<SymbolId>, Option<resolution_meta_json>).
///
/// Returns Err(String) for NotFound (with suggestions) or empty candidates.
/// Uses `c.symbol_id` directly — no store round-trip needed (Phase 1 fix).
pub(crate) fn resolution_to_symbol_ids_and_meta(
    resolution: &SymbolResolution,
    qname: &str,
) -> Result<(Vec<SymbolId>, Option<serde_json::Value>), String> {
    match resolution {
        SymbolResolution::Single {
            symbol_id,
            resolved,
        } => {
            let meta = json!({
                "policy": "aggregated",
                "count": 1,
                "matched_candidates": [{
                    "qualified_name": resolved.qualified_name,
                    "file_path": resolved.file_path,
                    "line": resolved.line,
                    "kind": resolved.kind,
                    "language": resolved.language,
                }],
            });
            Ok((vec![*symbol_id], Some(meta)))
        }
        SymbolResolution::Ambiguous { candidates, .. } => {
            let symbol_ids: Vec<SymbolId> = candidates.iter().map(|c| c.symbol_id).collect();
            if symbol_ids.is_empty() {
                Err(format!(
                    "Symbol '{qname}' resolved but no matching symbols found"
                ))
            } else {
                let meta = build_resolution_meta(candidates, symbol_ids.len());
                Ok((symbol_ids, Some(meta)))
            }
        }
        SymbolResolution::NotFound { qname, suggestions } => {
            let mut err = format!("Symbol not found: {qname}");
            if !suggestions.is_empty() {
                err.push_str(&format!(". Did you mean: {}?", suggestions.join(", ")));
            }
            Err(err)
        }
    }
}

/// Check if a SymbolKind represents a callable entity (can appear as the
/// source or target of a Calls edge).
fn is_callable_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
    )
}

/// Build a candidate JSON object for ambiguity reporting in path results.
/// Includes a `symbol_ref` that callers can use to disambiguate in subsequent
/// queries.
pub(crate) fn candidate_json(store: &Store, id: &SymbolId, selected: bool) -> serde_json::Value {
    store
        .find_symbol_by_id(id)
        .ok()
        .flatten()
        .map(|s| {
            let file_path = store
                .get_file(&s.file_id)
                .ok()
                .flatten()
                .map(|f| f.path)
                .unwrap_or_default();
            let line = s.range.start_line.saturating_add(1);
            json!({
                "qualified_name": s.qualified_name,
                "file": file_path,
                "line": line,
                "kind": s.kind.as_str(),
                "selected": selected,
                "symbol_ref": {
                    "qualified_name": s.qualified_name,
                    "file_path": file_path,
                    "line": line,
                    "kind": s.kind.as_str(),
                },
            })
        })
        .unwrap_or(json!({
            "qualified_name": "unknown",
            "file": "unknown",
            "line": 0,
            "kind": "unknown",
            "selected": selected,
        }))
}

/// Parse a snake_case edge kind string to an EdgeKind.
fn parse_edge_kind(s: &str) -> Result<EdgeKind, String> {
    match s {
        "calls" => Ok(EdgeKind::Calls),
        "instantiates" => Ok(EdgeKind::Instantiates),
        "implements" => Ok(EdgeKind::Implements),
        "registers_callback" => Ok(EdgeKind::RegistersCallback),
        "references" => Ok(EdgeKind::References),
        "contains" => Ok(EdgeKind::Contains),
        "imports" => Ok(EdgeKind::Imports),
        "includes" => Ok(EdgeKind::Includes),
        "exports" => Ok(EdgeKind::Exports),
        "extends" => Ok(EdgeKind::Extends),
        "typeof" => Ok(EdgeKind::TypeOf),
        "returns" => Ok(EdgeKind::Returns),
        "overrides" => Ok(EdgeKind::Overrides),
        "decorates" => Ok(EdgeKind::Decorates),
        "defines" => Ok(EdgeKind::Defines),
        "argument" => Ok(EdgeKind::Argument),
        "parameter" => Ok(EdgeKind::Parameter),
        "assigns" => Ok(EdgeKind::Assigns),
        "reads" => Ok(EdgeKind::Reads),
        "writes" => Ok(EdgeKind::Writes),
        "field_read" => Ok(EdgeKind::FieldRead),
        "field_write" => Ok(EdgeKind::FieldWrite),
        _ => Err(format!(
            "Unknown edge kind: '{s}'. Valid kinds: calls, instantiates, implements, registers_callback, references, contains, imports, includes, exports, extends, typeof, returns, overrides, decorates, defines, argument, parameter, assigns, reads, writes, field_read, field_write"
        )),
    }
}

impl ToolRouter {
    pub(crate) fn unresolved_call_refs_json(
        &self,
        source_ids: &[SymbolId],
        limit: usize,
    ) -> (Vec<serde_json::Value>, usize) {
        self.unresolved_call_refs_json_filtered(source_ids, limit, None)
    }

    fn unresolved_call_refs_json_filtered(
        &self,
        source_ids: &[SymbolId],
        limit: usize,
        target_name: Option<&str>,
    ) -> (Vec<serde_json::Value>, usize) {
        let store = &self.project().store;
        let mut seen = HashSet::new();
        let mut refs = Vec::new();
        let normalized_target = target_name.map(str::trim).filter(|s| !s.is_empty());

        for source_id in source_ids {
            let Ok(unresolved) = store.find_unresolved_call_references_by_source(source_id) else {
                continue;
            };
            for reference in unresolved {
                if let Some(target) = normalized_target {
                    let matches_target = reference.name == target
                        || reference.text == target
                        || reference.name.rsplit("::").next() == Some(target)
                        || reference.name.rsplit('.').next() == Some(target);
                    if !matches_target {
                        continue;
                    }
                }
                let line = reference.range.start_line.saturating_add(1);
                let key = format!("{}:{}:{}", reference.file_id.to_hex(), reference.name, line);
                if !seen.insert(key) {
                    continue;
                }
                let file = store
                    .get_file(&reference.file_id)
                    .ok()
                    .flatten()
                    .map(|f| f.path)
                    .unwrap_or_default();
                refs.push(json!({
                    "name": reference.name,
                    "text": reference.text,
                    "file": file,
                    "line": line,
                    "column": reference.range.start_column,
                    "kind": "unresolved_call",
                    "resolution": "unresolved",
                }));
            }
        }

        refs.sort_by(|a, b| {
            let af = a.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let bf = b.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let al = a.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let bl = b.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            af.cmp(bf).then(al.cmp(&bl))
        });
        let total = refs.len();
        refs.truncate(limit);
        (refs, total)
    }

    pub(crate) fn unresolved_call_target_hint(
        &self,
        source_ids: &[SymbolId],
        target_name: &str,
    ) -> Option<String> {
        let (matches, total) =
            self.unresolved_call_refs_json_filtered(source_ids, 3, Some(target_name));
        if matches.is_empty() {
            return None;
        }

        let locations = matches
            .iter()
            .filter_map(|m| {
                let file = m.get("file")?.as_str()?;
                let line = m.get("line")?.as_u64()?;
                let column = m.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                Some(format!("{file}:{line}:{column}"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if total > matches.len() {
            format!(" and {} more callsite(s)", total - matches.len())
        } else {
            String::new()
        };

        Some(format!(
            " Target '{target_name}' appears as an unresolved call token from the source at {locations}{suffix}. This is usually an external helper, macro, builtin, or a symbol outside the current focus/full index. Use calls(direction=\"outgoing\") to inspect unresolved_callees, or trace(kind=\"point\") at the callsite. path/trace forward require both endpoints to resolve to local symbols."
        ))
    }

    pub(crate) fn resolve_graph_symbol_with_focus_retry(
        &self,
        input: &SymbolInput,
        policy: SymbolResolutionPolicy,
        direction: Option<String>,
        depth: Option<usize>,
        include_roots: &[atlas_engine::IncludeRoot],
    ) -> Result<SymbolResolution, String> {
        let qname = symbol_input_qname(input);
        let resolution = self.resolve_symbol_input(input, policy)?;
        if !matches!(resolution, SymbolResolution::NotFound { .. }) {
            return Ok(resolution);
        }

        let selector_file_id = self.resolve_selector_file_id(input);
        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: selector_file_id,
            symbol_id: None,
            direction,
            depth,
        });
        let _ = self.prepare_focus_query_with_roots(intent, include_roots.to_vec());
        self.resolve_symbol_input(input, policy)
    }
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
