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

/// Default edge kinds for impact analysis: calls, instantiates, implements,
/// callback registrations, imports, and includes.
const DEFAULT_IMPACT_EDGES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::Implements,
    EdgeKind::RegistersCallback,
    EdgeKind::Imports,
    EdgeKind::Includes,
];

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

    fn scoped_explore_resolution(
        &self,
        qname: &str,
        scope: &str,
        include_roots: Vec<String>,
    ) -> Result<Option<SymbolResolution>, String> {
        let structural = self.project().materialize.structural().clone();
        let svc = ScopedSearchService::new_with_project_root(
            self.project().store.clone(),
            structural,
            Some(self.project().root.clone()),
        );
        let resp = svc
            .execute(ScopedSearchRequest {
                query: qname.to_string(),
                scope: Some(scope.to_string()),
                analysis: SearchAnalysis::Auto,
                limit: MAX_AMBIGUOUS_CANDIDATES,
                include_roots,
                ..Default::default()
            })
            .map_err(|e| format!("Scoped explore search failed: {e}"))?;

        if resp.results.is_empty() {
            return Ok(None);
        }

        let mut exact: Vec<SymbolDef> = resp
            .results
            .iter()
            .filter(|hit| hit.symbol.name == qname || hit.symbol.qualified_name == qname)
            .map(|hit| hit.symbol.clone())
            .collect();
        if exact.is_empty() {
            exact = resp.results.iter().map(|hit| hit.symbol.clone()).collect();
        }

        if exact.len() == 1 {
            let sym = exact.remove(0);
            let file_path = self
                .project()
                .store_query_runtime
                .resolve_file_path(&sym.file_id);
            let line = sym.range.start_line.saturating_add(1);
            return Ok(Some(SymbolResolution::Single {
                symbol_id: sym.id,
                resolved: crate::tools::symbol_selector::ResolvedSymbol {
                    qualified_name: sym.qualified_name,
                    file_path,
                    line,
                    kind: sym.kind.as_str().to_string(),
                    language: sym.language.as_str().to_string(),
                    match_info: MatchInfo {
                        mode: MatchMode::UniqueQname,
                        ignored_mismatches: Vec::new(),
                        path_match: None,
                        line_delta: None,
                    },
                },
            }));
        }

        let candidates = exact
            .iter()
            .take(MAX_AMBIGUOUS_CANDIDATES)
            .map(|sym| {
                let file_path = self
                    .project()
                    .store_query_runtime
                    .resolve_file_path(&sym.file_id);
                let line = sym.range.start_line.saturating_add(1);
                ScoredCandidate {
                    qualified_name: sym.qualified_name.clone(),
                    file_path: file_path.clone(),
                    line,
                    kind: sym.kind.as_str().to_string(),
                    language: sym.language.as_str().to_string(),
                    score: 9_000,
                    reasons: vec![format!("scope:{scope}")],
                    symbol_ref: SymbolSelector {
                        qualified_name: sym.qualified_name.clone(),
                        file_path: Some(file_path),
                        line: Some(line),
                        kind: Some(sym.kind.as_str().to_string()),
                        language: Some(sym.language.as_str().to_string()),
                    },
                    symbol_id: sym.id,
                }
            })
            .collect();
        Ok(Some(SymbolResolution::Ambiguous {
            candidates,
            score_gap: 0,
        }))
    }

    pub(crate) fn handle_explore(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = super::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        let include_roots_for_scope: Vec<String> =
            include_roots.iter().map(|root| root.path.clone()).collect();
        let source_mode = parse_source_mode(args);
        let source_lines = args
            .get("source_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(40) as u32;
        let evidence_limit = args
            .get("evidence_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let relation_limit = args
            .get("relation_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(12) as usize;
        let peer_limit = args
            .get("peer_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(12) as usize;
        let max_source_bytes: usize = 65536;
        let include_file_context = args
            .get("include_file_context")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let include_recommendations = args
            .get("include_recommendations")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let scope = get_str_opt(args, "scope")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let has_file_hint = matches!(
            &input,
            SymbolInput::Selector(sel)
                if sel.file_path.as_deref().is_some_and(|p| !p.trim().is_empty())
        );
        let prepared_focus = if scope.is_none() && has_file_hint {
            Some(self.prepare_focus_query_with_roots(
                Some(atlas_engine::QueryIntent::Explore {
                    symbol_name: qname.to_string(),
                    file_id: self.resolve_selector_file_id(&input),
                    symbol_id: None,
                }),
                include_roots.clone(),
            ))
        } else {
            None
        };

        // Resolve with UniqueOrCandidates policy.
        let resolution = if let Some(scope) = scope {
            match self.scoped_explore_resolution(qname, scope, include_roots_for_scope) {
                Ok(Some(r)) => r,
                Ok(None) => SymbolResolution::NotFound {
                    qname: qname.to_string(),
                    suggestions: Vec::new(),
                },
                Err(e) => return (e, true),
            }
        } else {
            match self.resolve_symbol_input(&input, SymbolResolutionPolicy::UniqueOrCandidates) {
                Ok(r) => r,
                Err(e) => return (e, true),
            }
        };

        let lr = AnalysisEnvelope::new("explore", args).with_root_warnings(root_warnings);

        let (sym_id, mut resolved_opt) = match resolution {
            SymbolResolution::Single {
                symbol_id,
                resolved,
            } => (symbol_id, Some(resolved)),
            SymbolResolution::Ambiguous {
                candidates,
                score_gap,
            } => {
                let candidate_list: Vec<serde_json::Value> = candidates
                    .iter()
                    .map(|c| {
                        json!({
                            "qualified_name": c.qualified_name,
                            "file_path": c.file_path,
                            "line": c.line,
                            "kind": c.kind,
                            "language": c.language,
                            "score": c.score,
                            "reasons": c.reasons,
                            "symbol_ref": c.symbol_ref,
                        })
                    })
                    .collect();
                let resp = json!({
                    "symbol": qname,
                    "ambiguous": true,
                    "score_gap": score_gap,
                    "candidates": candidate_list,
                });
                return lr.with_is_error(false).build(resp, self);
            }
            SymbolResolution::NotFound {
                ref qname,
                ref suggestions,
            } => {
                let mut resp = json!({
                    "symbol": qname,
                    "status": "unresolved",
                    "message": "The symbol is not available in the current local focus closure yet. Background scoped analysis has been started; retry this explore request after the suggested delay, or pass a SymbolSelector with file_path/scope to constrain the local region.",
                });
                if !suggestions.is_empty() {
                    resp["suggestions"] = json!(suggestions);
                }
                if let Some(scope) = scope {
                    resp["scope"] = json!(scope);
                }
                let (background_jobs, candidate_files) = if let Some(scope) = scope {
                    let file_ids = self
                        .project()
                        .store
                        .list_file_inventory_ids_in_scope(scope, 24)
                        .unwrap_or_default();
                    (self.enqueue_background_file_focus(&file_ids), Vec::new())
                } else {
                    let file_ids = self.candidate_file_ids_for_symbol(qname);
                    let files = self.candidate_file_paths(&file_ids);
                    (self.enqueue_background_file_focus(&file_ids), files)
                };
                if !candidate_files.is_empty() {
                    resp["candidate_files"] = json!(candidate_files);
                };
                let summary = if background_jobs.is_empty() {
                    "explore returned a bounded unresolved result; retry after focus bootstrap has warmed more inventory"
                } else {
                    "explore returned a bounded unresolved result; background scoped analysis is preparing local symbol facts"
                };
                return lr
                    .with_is_error(false)
                    .with_analysis_scope("local".into())
                    .with_analysis_summary(summary.into())
                    .with_analysis_basis(vec!["manifest".into(), "structural".into()])
                    .with_analysis_retry_after_ms(2000)
                    .build(resp, self);
            }
        };

        let seed_sym = match self.project().store.find_symbol_by_id(&sym_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let mut err = format!("Symbol not found in store: {qname}");
                err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                return (err, true);
            }
            Err(e) => {
                let mut err = format!("Lookup error: {e}");
                err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                return (err, true);
            }
        };

        self.update_investigation(InvestigationFocus::Symbol(seed_sym.id));

        // Lazy structural: prepare focus query for graph edges
        let (focus_result, focus_warnings) = prepared_focus.unwrap_or_else(|| {
            self.prepare_focus_query_with_roots(
                Some(atlas_engine::QueryIntent::Explore {
                    symbol_name: qname.to_string(),
                    file_id: Some(seed_sym.file_id),
                    symbol_id: None,
                }),
                include_roots,
            )
        });

        // Focus may replace manifest or stale structural facts for the same
        // deterministic SymbolId. Never build the dossier from the pre-focus
        // copy: its range can be only the declaration name even though the DB
        // now contains the complete definition.
        let sym = match self.project().store.find_symbol_by_id(&sym_id) {
            Ok(Some(symbol)) => symbol,
            Ok(None) => {
                return (
                    format!("Symbol disappeared after focus preparation: {qname}"),
                    true,
                );
            }
            Err(error) => {
                return (
                    format!("Lookup after focus preparation failed: {error}"),
                    true,
                );
            }
        };
        if let Ok(SymbolResolution::Single { resolved, .. }) =
            self.resolve_symbol_input(&input, SymbolResolutionPolicy::UniqueOrCandidates)
        {
            resolved_opt = Some(resolved);
        }
        let file_path = self
            .project()
            .store_query_runtime
            .resolve_file_path(&sym.file_id);

        let (store_clone, root_clone) = {
            let active = self.project();
            (active.store.clone(), active.root.clone())
        };
        let sym_repo = atlas_engine::dossier::SymbolRepo::new(store_clone.clone());
        let source_repo = atlas_engine::dossier::SourceRepo::new(store_clone.clone(), root_clone);
        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let relation_repo = atlas_engine::dossier::RelationRepo::new(store_clone.clone(), graph);
        let file_repo = atlas_engine::dossier::FileFactsRepo::new(store_clone);

        let request = atlas_engine::dossier::types::ExploreRequest {
            symbol: qname.to_string(),
            source_mode,
            source_lines,
            evidence_limit,
            relation_limit,
            peer_limit,
            max_source_bytes,
            include_file_context,
            include_recommendations,
        };

        let tier_str = "unknown".to_string();

        let mut dossier = match atlas_engine::dossier::builder::ExploreDossierBuilder::build(
            &sym,
            &file_path,
            &sym_repo,
            &relation_repo,
            &file_repo,
            &source_repo,
            &request,
            tier_str,
        ) {
            Ok(d) => d,
            Err(e) => return (format!("Failed to build dossier: {e}"), true),
        };

        source_repo.clear_cache();

        // Merge focus warnings into dossier warnings
        dossier.warnings.extend(focus_warnings);

        let mut resp_value =
            serde_json::to_value(&dossier).unwrap_or_else(|e| json!({"error": e.to_string()}));
        if let Some(response) = resp_value.as_object_mut() {
            response.remove("precisionTier");
        }

        // Include resolution metadata when resolved
        if let Some(ref resolved) = resolved_opt {
            resp_value["resolution"] = json!({
                "policy": "unique_or_candidates",
                "resolved": resolved,
            });
        }

        let lr = lr.with_lazy_warnings(dossier.warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp_value, args, self)
    }

    pub(crate) fn handle_impact(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = super::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;
        let semantic = args
            .get("semantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Parse optional include_children flag.
        let include_children = args
            .get("include_children")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Parse optional direction parameter.
        let direction_str = args
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("outgoing");
        let direction = match direction_str {
            "outgoing" => TraversalDirection::Outgoing,
            "incoming" => TraversalDirection::Incoming,
            "both" => TraversalDirection::Both,
            other => {
                return (
                    format!("direction must be 'outgoing', 'incoming', or 'both', got: {other}"),
                    true,
                );
            }
        };

        // Parse optional edge_kinds override.
        let edge_kinds: Option<Vec<EdgeKind>> = match args.get("edge_kinds") {
            None | Some(serde_json::Value::Null) => None,
            Some(raw) => {
                let arr = match raw.as_array() {
                    Some(a) => a,
                    None => {
                        return ("edge_kinds must be an array of strings".to_string(), true);
                    }
                };
                if arr.is_empty() || (arr.len() == 1 && arr[0].as_str() == Some("*")) {
                    Some(vec![])
                } else {
                    let mut kinds = Vec::with_capacity(arr.len());
                    for v in arr {
                        let s = v.as_str().unwrap_or("");
                        if s == "*" {
                            return ("'*' must be the only value in edge_kinds".to_string(), true);
                        }
                        kinds.push(match parse_edge_kind(s) {
                            Ok(k) => k,
                            Err(e) => return (e, true),
                        });
                    }
                    Some(kinds)
                }
            }
        };

        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::Aggregate,
            None,
            Some(depth),
            &[],
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) =
            match resolution_to_symbol_ids_and_meta(&resolution, qname) {
                Ok(r) => r,
                Err(e) => {
                    if let Some(qname) = not_found_resolution_qname(&resolution) {
                        return self.retryable_symbol_not_found_response(
                            "impact",
                            args,
                            qname,
                            Vec::new(),
                            Some("impact requires the root symbol to be materialized first".into()),
                        );
                    }
                    return (e, true);
                }
            };

        let sid = symbol_ids[0];

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = AnalysisEnvelope::new("impact", args);

        let intent = Some(atlas_engine::QueryIntent::Impact {
            symbol_name: qname.to_string(),
            depth: Some(depth),
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);

        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let sub = if include_children {
            graph.impact_with_children_and_kinds(&sid, depth.min(5), edge_kinds.clone(), direction)
        } else {
            graph.impact_with_kinds(&sid, depth.min(5), edge_kinds.clone(), direction)
        };
        let snap = graph.snapshot();

        // Determine which edge kinds were actually used for the response.
        let edge_kinds_used: Vec<&str> = match &edge_kinds {
            None => DEFAULT_IMPACT_EDGES.iter().map(|k| k.as_str()).collect(),
            Some(kinds) if kinds.is_empty() => vec!["*"],
            Some(kinds) => kinds.iter().map(|k| k.as_str()).collect(),
        };

        // Group impacted nodes by file for hierarchical output.
        let mut file_groups: std::collections::HashMap<
            atlas_engine::FileId,
            Vec<serde_json::Value>,
        > = std::collections::HashMap::new();
        let mut total_shown = 0usize;

        for &ix in &sub.node_indices {
            if total_shown >= 30 {
                break;
            }
            let node = snap.node(ix);
            file_groups.entry(node.file_id).or_default().push(json!({
                "name": node.name,
                "qualified_name": node.qualified_name,
                "kind": node.kind.as_str(),
                "line": node.start_line,
            }));
            total_shown += 1;
        }

        let grouped: Vec<_> = file_groups
            .into_iter()
            .map(|(fid, symbols)| {
                json!({
                    "file": self.project().store_query_runtime.resolve_file_path(&fid),
                    "symbols": symbols,
                })
            })
            .collect();

        let semantic_impact = semantic.then(|| {
            let target_ids = sub
                .node_indices
                .iter()
                .map(|&ix| snap.node(ix).symbol_id)
                .collect::<Vec<_>>();
            project
                .analysis_runtime
                .run_semantic_impact(&project.store, &target_ids)
        });

        let total_reached = sub.node_indices.len();
        let truncated = total_reached > total_shown || total_reached >= 1000;
        let mut resp = json!({
            "symbol": qname,
            "max_depth": depth,
            "total_reached": total_reached,
            "shown": total_shown,
            "truncated": truncated,
            "bfs_limit": 1000,
            "file_groups": grouped,
            "edge_kinds_used": edge_kinds_used,
            "include_children": include_children,
            "direction": direction_str,
        });
        if let Some(rm) = resolution_meta_opt {
            resp["resolution"] = rm;
        }
        if let Some(semantic_impact) = semantic_impact {
            resp["semantic_impact"] = json!(semantic_impact);
        }
        {
            let active = self.project();
            if !active.query_runtime.has_full_index(&active.store) {
                resp["capability_note"] = json!(
                    "focus mode: impact is bounded by the current focus closure; background refinement may discover additional affected symbols."
                );
                resp["note"] = json!(
                    "Impact is complete only within the current focus closure. Use CLI `atlas index --analysis full` only when you want an explicit project-wide cache."
                );
            }
        }

        if direction == TraversalDirection::Both
            && edge_kinds_used
                .iter()
                .any(|k| *k == "imports" || *k == "includes")
        {
            resp["noise_note"] = json!(
                "Bidirectional traversal with imports/includes may include unrelated consumer modules. Consider direction='outgoing' for narrower impact radius."
            );
        }

        let lr = lr
            .with_root_warnings(Vec::new())
            .with_lazy_warnings(focus_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build(resp, self)
    }
}

/// Parse the `source_mode` parameter from explore tool arguments.
fn parse_source_mode(args: &serde_json::Value) -> atlas_engine::dossier::types::SourceMode {
    match args.get("source_mode").and_then(|v| v.as_str()) {
        Some("full") => atlas_engine::dossier::types::SourceMode::Full,
        Some("none") => atlas_engine::dossier::types::SourceMode::None_,
        _ => atlas_engine::dossier::types::SourceMode::Excerpt,
    }
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
