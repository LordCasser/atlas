//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use std::collections::HashSet;
use std::sync::Arc;

use atlas_engine::analysis;
use atlas_engine::dossier::SourceRepository;
use atlas_engine::symbol_selector::{MatchInfo, MatchMode};
use atlas_engine::{
    EdgeKind, Engine, InvestigationFocus, ScopedSearchRequest, ScopedSearchService, SearchAnalysis,
    Store, SymbolDef, SymbolId, SymbolKind, TraversalDirection,
};

use super::analysis_envelope::AnalysisEnvelope;
use super::{MAX_AMBIGUOUS_CANDIDATES, ToolRouter, get_str, get_str_opt, get_u64};
use crate::tools::symbol_selector::{
    ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy, SymbolSelector,
    parse_symbol_input,
};

use serde_json::json;

/// Check whether an edge kind is allowed by a configurable filter.
/// An empty `allowed` slice means *all* edge kinds are allowed.
fn is_allowed_edge(kind: &EdgeKind, allowed: &[EdgeKind]) -> bool {
    if allowed.is_empty() {
        return true; // wildcard / all edges
    }
    allowed.contains(kind)
}

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

/// Build resolution metadata for path endpoint responses.
fn build_resolution_meta_for_path(resolution: &SymbolResolution) -> serde_json::Value {
    match resolution {
        SymbolResolution::Single { resolved, .. } => {
            json!({
                "policy": "aggregated",
                "count": 1,
                "matched_candidates": [{
                    "qualified_name": resolved.qualified_name,
                    "file_path": resolved.file_path,
                    "line": resolved.line,
                    "kind": resolved.kind,
                    "language": resolved.language,
                }],
            })
        }
        SymbolResolution::Ambiguous { candidates, .. } => {
            build_resolution_meta(candidates, candidates.len())
        }
        SymbolResolution::NotFound { qname, .. } => {
            json!({
                "policy": "aggregated",
                "count": 0,
                "matched_candidates": [],
                "qname": qname,
            })
        }
    }
}

/// Default edge kinds for call-graph traversal (call relationships).
const DEFAULT_CALL_EDGES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::Implements,
];

/// Parse the `edge_kinds` argument for call-graph tools.
/// Returns the list of allowed edge kinds; an empty vec means "all edges" (wildcard).
fn resolve_call_edge_kinds(args: &serde_json::Value) -> Result<Vec<EdgeKind>, String> {
    let raw = match args.get("edge_kinds") {
        None | Some(serde_json::Value::Null) => return Ok(DEFAULT_CALL_EDGES.to_vec()),
        Some(v) => v,
    };
    let arr = raw
        .as_array()
        .ok_or_else(|| "edge_kinds must be an array of strings".to_string())?;
    if arr.is_empty() {
        return Ok(vec![]); // all edge kinds
    }
    if arr.len() == 1 && arr[0].as_str() == Some("*") {
        return Ok(vec![]); // wildcard → all edge kinds
    }
    let mut kinds = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v.as_str().unwrap_or("");
        if s == "*" {
            return Err("'*' must be the only value in edge_kinds".to_string());
        }
        kinds.push(parse_edge_kind(s)?);
    }
    Ok(kinds)
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

/// Default edge kinds for path finding — call relationships only.
/// Excludes non-control-flow edges (References, TypeOf, Contains, etc.)
/// to avoid semantically meaningless paths in security analysis.
const DEFAULT_PATH_EDGES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::Implements,
    EdgeKind::RegistersCallback,
];

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
    fn candidate_outgoing_neighbors(
        &self,
        root_id: &SymbolId,
        allowed_edge_kinds: &[EdgeKind],
    ) -> Vec<SymbolId> {
        let Ok(candidates) = self
            .project()
            .store
            .find_visible_candidate_edges_by_source(root_id)
        else {
            return Vec::new();
        };

        candidates
            .into_iter()
            .filter_map(|candidate| {
                let edge_kind = parse_edge_kind(&candidate.kind).ok()?;
                if !is_allowed_edge(&edge_kind, allowed_edge_kinds) {
                    return None;
                }
                let target = candidate.target?;
                let bytes: [u8; 32] = target.as_slice().try_into().ok()?;
                Some(SymbolId::from_bytes(bytes))
            })
            .collect()
    }

    fn candidate_incoming_neighbors(
        &self,
        root_id: &SymbolId,
        allowed_edge_kinds: &[EdgeKind],
    ) -> Vec<SymbolId> {
        let Ok(candidates) = self
            .project()
            .store
            .find_visible_candidate_edges_by_target(root_id)
        else {
            return Vec::new();
        };

        candidates
            .into_iter()
            .filter_map(|candidate| {
                let edge_kind = parse_edge_kind(&candidate.kind).ok()?;
                if !is_allowed_edge(&edge_kind, allowed_edge_kinds) {
                    return None;
                }
                let bytes: [u8; 32] = candidate.source.as_slice().try_into().ok()?;
                Some(SymbolId::from_bytes(bytes))
            })
            .collect()
    }

    fn symbol_json_by_id(&self, symbol_id: &SymbolId) -> Option<serde_json::Value> {
        let project = self.project();
        let sym = project.store.find_symbol_by_id(symbol_id).ok().flatten()?;
        Some(json!({
            "name": sym.name,
            "qualified_name": sym.qualified_name,
            "kind": sym.kind.as_str(),
            "file": project.store_query_runtime.resolve_file_path(&sym.file_id),
            "line": sym.range.start_line.saturating_add(1),
        }))
    }

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
        let _ = self.prepare_focus_query(intent);
        self.resolve_symbol_input(input, policy)
    }

    fn scoped_explore_resolution(
        &self,
        qname: &str,
        scope: &str,
    ) -> Result<Option<SymbolResolution>, String> {
        let engine: Arc<Engine> = Arc::new(Engine::from_store(
            self.project().store.clone(),
            Some(&self.project().root),
        ));
        let svc = ScopedSearchService::new_with_project_root(
            self.project().store.clone(),
            engine,
            Some(self.project().root.clone()),
        );
        let resp = svc
            .execute(ScopedSearchRequest {
                query: qname.to_string(),
                scope: Some(scope.to_string()),
                analysis: SearchAnalysis::Auto,
                limit: MAX_AMBIGUOUS_CANDIDATES,
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

    pub(crate) fn handle_callers(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = super::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::Aggregate,
            Some("incoming".to_string()),
            None,
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) = match resolution_to_symbol_ids_and_meta(
            &resolution,
            qname,
        ) {
            Ok(r) => r,
            Err(e) => {
                if let Some(qname) = not_found_resolution_qname(&resolution) {
                    return self.retryable_symbol_not_found_response(
                            "calls",
                            args,
                            qname,
                            Vec::new(),
                            Some("calls(direction=incoming) requires the symbol to be materialized first".into()),
                        );
                }
                return (e, true);
            }
        };

        let sid = symbol_ids[0];
        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = AnalysisEnvelope::new("calls", args);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: self.resolve_selector_file_id(&input),
            symbol_id: Some(sid),
            direction: Some("incoming".to_string()),
            depth: None,
        });
        let (focus_result, mut focus_warnings) = self.prepare_focus_query(intent);

        let has_full_index = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };

        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let snap = graph.snapshot();
        let call_edge_kinds = DEFAULT_CALL_EDGES.to_vec();

        // Multi-root: union of callers from all matched SymbolIds, deduplicated.
        let mut seen: HashSet<SymbolId> = HashSet::new();
        let mut all_callers: Vec<serde_json::Value> = Vec::new();
        for &root_id in &symbol_ids {
            let cg = graph.callers(&root_id);
            for &ix in &cg.callers {
                let caller_id = snap.node(ix).symbol_id;
                if seen.insert(caller_id) {
                    all_callers.push(super::node_json(
                        &self.project().store_query_runtime,
                        snap,
                        ix,
                        None,
                    ));
                }
            }
            if !has_full_index {
                for caller_id in self.candidate_incoming_neighbors(&root_id, &call_edge_kinds) {
                    if seen.insert(caller_id) {
                        if let Some(node) = self.symbol_json_by_id(&caller_id) {
                            all_callers.push(node);
                        }
                    }
                }
            }
        }
        // ── callers ────────────────────────────────────────────────────
        let total_callers = all_callers.len();
        let nodes: Vec<_> = all_callers.into_iter().take(limit).collect();

        let mut resp = json!({
            "symbol": qname,
            "total_callers": total_callers,
            "callers": nodes,
        });
        if let Some(rm) = resolution_meta_opt {
            if symbol_ids.len() > 1 {
                resp["resolution"] = rm;
            }
        }
        if !has_full_index {
            resp["note"] = json!(
                "Incoming calls are complete only within the current focus closure. Background refinement may discover additional callers outside this closure."
            );
            focus_warnings.push(
                "Incoming call results are scoped to the current focus closure; absence of additional callers is not a repo-wide proof until full indexing completes."
                    .to_string(),
            );
        }

        // Lazy structural response with focus-aware envelope
        let lr = lr.with_lazy_warnings(focus_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp, args, self)
    }

    pub(crate) fn handle_callees(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = super::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::Aggregate,
            Some("outgoing".to_string()),
            None,
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) = match resolution_to_symbol_ids_and_meta(
            &resolution,
            qname,
        ) {
            Ok(r) => r,
            Err(e) => {
                if let Some(qname) = not_found_resolution_qname(&resolution) {
                    return self.retryable_symbol_not_found_response(
                            "calls",
                            args,
                            qname,
                            Vec::new(),
                            Some("calls(direction=outgoing) requires the symbol to be materialized first".into()),
                        );
                }
                return (e, true);
            }
        };

        let sid = symbol_ids[0];
        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = AnalysisEnvelope::new("calls", args);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: self.resolve_selector_file_id(&input),
            symbol_id: Some(sid),
            direction: Some("outgoing".to_string()),
            depth: None,
        });
        let (focus_result, mut focus_warnings) = self.prepare_focus_query(intent);

        let has_full_index = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };

        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let snap = graph.snapshot();
        let call_edge_kinds = DEFAULT_CALL_EDGES.to_vec();

        // Multi-root: union of callees from all matched SymbolIds, deduplicated.
        let mut seen: HashSet<SymbolId> = HashSet::new();
        let mut all_callees: Vec<serde_json::Value> = Vec::new();
        for &root_id in &symbol_ids {
            let cg = graph.callees(&root_id);
            for &ix in &cg.callees {
                let callee_id = snap.node(ix).symbol_id;
                if seen.insert(callee_id) {
                    all_callees.push(super::node_json(
                        &self.project().store_query_runtime,
                        snap,
                        ix,
                        None,
                    ));
                }
            }
            if !has_full_index {
                for callee_id in self.candidate_outgoing_neighbors(&root_id, &call_edge_kinds) {
                    if seen.insert(callee_id) {
                        if let Some(node) = self.symbol_json_by_id(&callee_id) {
                            all_callees.push(node);
                        }
                    }
                }
            }
        }
        // ── callees ────────────────────────────────────────────────────
        let total_callees = all_callees.len();
        let nodes: Vec<_> = all_callees.into_iter().take(limit).collect();
        let (unresolved_callees, total_unresolved_callees) =
            self.unresolved_call_refs_json(&symbol_ids, limit);

        let mut resp = json!({
            "symbol": qname,
            "total_callees": total_callees,
            "callees": nodes,
        });
        if total_unresolved_callees > 0 {
            resp["total_unresolved_callees"] = json!(total_unresolved_callees);
            resp["unresolved_callees"] = json!(unresolved_callees);
            resp["unresolved_callee_note"] = json!(
                "These call tokens were extracted from the function body but did not resolve to local symbols. They may be macros, builtins, external helpers, or code outside the current focus/full index."
            );
            if !has_full_index {
                focus_warnings.push(format!(
                    "{total_unresolved_callees} outgoing call token(s) remain unresolved in the current focus closure."
                ));
            }
        }
        if let Some(rm) = resolution_meta_opt {
            if symbol_ids.len() > 1 {
                resp["resolution"] = rm;
            }
        }
        if !has_full_index {
            resp["note"] = json!(
                "Outgoing calls are complete only within the current focus closure. Unresolved callees mark the refinement frontier."
            );
        }

        // Lazy structural response with focus-aware envelope
        let lr = lr.with_lazy_warnings(focus_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp, args, self)
    }

    pub(crate) fn handle_callgraph(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = super::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;
        let limit = get_u64(args, "limit").unwrap_or(100) as usize;

        let direction = get_str(args, "direction");
        let edge_kinds = match resolve_call_edge_kinds(args) {
            Ok(k) => k,
            Err(e) => return (e, true),
        };

        let retry_direction = if direction.is_empty() {
            None
        } else {
            Some(direction.to_string())
        };
        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::Aggregate,
            retry_direction.clone(),
            Some(depth),
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
                        "calls",
                        args,
                        qname,
                        Vec::new(),
                        Some(
                            "callgraph traversal requires the root symbol to be materialized first"
                                .into(),
                        ),
                    );
                    }
                    return (e, true);
                }
            };

        let sid = symbol_ids[0];
        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = AnalysisEnvelope::new("calls", args);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: self.resolve_selector_file_id(&input),
            symbol_id: Some(sid),
            direction: retry_direction,
            depth: Some(depth),
        });
        let (focus_result, lazy_warnings) = self.prepare_focus_query(intent);

        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let snap = graph.snapshot();

        // Build hop-by-hop view: multi-root BFS.
        let mut hops: Vec<serde_json::Value> = Vec::new();
        let mut total_nodes = 0usize;

        // Hop 0: all root symbol(s)
        let mut root_nodes: Vec<serde_json::Value> = Vec::new();
        let mut visited: HashSet<SymbolId> = HashSet::new();
        let mut frontier: Vec<SymbolId> = Vec::new();
        for &root_id in &symbol_ids {
            if let Some(ix) = snap.id_to_idx.get(&root_id).copied() {
                root_nodes.push(super::node_json(
                    &self.project().store_query_runtime,
                    snap,
                    ix,
                    None,
                ));
                visited.insert(root_id);
                frontier.push(root_id);
                total_nodes += 1;
            }
        }
        if root_nodes.is_empty() {
            return (
                format!("symbol '{qname}' not found in graph snapshot"),
                true,
            );
        }
        if root_nodes.len() == 1 {
            hops.push(json!({
                "depth": 0,
                "symbol": &root_nodes[0],
                "callers": [],
                "callees": [],
            }));
        } else {
            hops.push(json!({
                "depth": 0,
                "roots": root_nodes,
                "callers": [],
                "callees": [],
            }));
        }

        for d in 1..=depth.min(5) {
            if total_nodes >= limit {
                break;
            }
            let mut next_frontier: Vec<SymbolId> = Vec::new();
            let mut hop_callers: Vec<serde_json::Value> = Vec::new();
            let mut hop_callees: Vec<serde_json::Value> = Vec::new();

            // Respect direction filter: skip incoming/outgoing when direction
            // is explicitly set to the opposite.
            let want_incoming =
                direction.is_empty() || direction == "both" || direction == "incoming";
            let want_outgoing =
                direction.is_empty() || direction == "both" || direction == "outgoing";

            for fid in &frontier {
                if want_incoming {
                    // Incoming edges → callers
                    for (neighbor_ix, edge_kind) in snap.incoming_neighbors_with_kinds(fid) {
                        let neighbor_id = snap.node(neighbor_ix).symbol_id;
                        if visited.contains(&neighbor_id) {
                            continue;
                        }
                        // Only include edges matching the configured edge_kinds filter
                        if !is_allowed_edge(&edge_kind, &edge_kinds) {
                            continue;
                        }
                        visited.insert(neighbor_id);
                        next_frontier.push(neighbor_id);
                        hop_callers.push(json!({
                            "name": snap.node(neighbor_ix).name,
                            "qualified_name": snap.node(neighbor_ix).qualified_name,
                            "kind": snap.node(neighbor_ix).kind.as_str(),
                            "edge": edge_kind.as_str(),
                            "file": self.project().store_query_runtime.resolve_file_path(&snap.node(neighbor_ix).file_id),
                            "line": snap.node(neighbor_ix).start_line,
                        }));
                    }
                }
                if want_outgoing {
                    // Outgoing edges → callees
                    for (neighbor_ix, edge_kind) in snap.outgoing_neighbors_with_kinds(fid) {
                        let neighbor_id = snap.node(neighbor_ix).symbol_id;
                        if visited.contains(&neighbor_id) {
                            continue;
                        }
                        if !is_allowed_edge(&edge_kind, &edge_kinds) {
                            continue;
                        }
                        visited.insert(neighbor_id);
                        next_frontier.push(neighbor_id);
                        hop_callees.push(json!({
                            "name": snap.node(neighbor_ix).name,
                            "qualified_name": snap.node(neighbor_ix).qualified_name,
                            "kind": snap.node(neighbor_ix).kind.as_str(),
                            "edge": edge_kind.as_str(),
                            "file": self.project().store_query_runtime.resolve_file_path(&snap.node(neighbor_ix).file_id),
                            "line": snap.node(neighbor_ix).start_line,
                        }));
                    }
                }
            }

            // Split remaining budget evenly between callers and callees.
            let remaining = limit.saturating_sub(total_nodes);
            let half = remaining / 2;
            hop_callers.truncate(half);
            hop_callees.truncate(remaining.saturating_sub(half));
            total_nodes = total_nodes
                .saturating_add(hop_callers.len())
                .saturating_add(hop_callees.len());

            hops.push(json!({
                "depth": d,
                "callers": hop_callers,
                "callees": hop_callees,
            }));

            frontier = next_frontier;
        }

        let mut resp = json!({
            "symbol": qname,
            "max_depth": depth,
            "total_nodes_visited": total_nodes,
            "hops": hops,
        });
        if let Some(rm) = resolution_meta_opt {
            if symbol_ids.len() > 1 {
                resp["resolution"] = rm;
            }
        }
        {
            let active = self.project();
            if !active.query_runtime.has_full_index(&active.store) {
                resp["note"] = json!(
                    "Graph expansion is complete only within the current focus closure. Background refinement may discover additional edges outside this closure; use CLI `atlas index --analysis full` only when you want an explicit project-wide cache."
                );
            }
        }

        let lr = lr.with_lazy_warnings(lazy_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp, args, self)
    }

    pub(crate) fn handle_path(&self, args: &serde_json::Value) -> (String, bool) {
        // Parse 'from' and 'to' as SymbolInput (string or selector object).
        let from_input = match parse_symbol_field(args, "from") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let to_input = match parse_symbol_field(args, "to") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let from_qname = symbol_input_qname(&from_input);
        let to_qname = symbol_input_qname(&to_input);
        if let Err(e) = super::validate_symbol_name_length(from_qname) {
            return (e, true);
        }
        if let Err(e) = super::validate_symbol_name_length(to_qname) {
            return (e, true);
        }
        let max_depth = get_u64(args, "max_depth").unwrap_or(5) as usize;
        let prefer_production = args
            .get("prefer_production")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let direction = Self::resolve_path_direction(args);

        let edge_kind_filter = match Self::resolve_path_edge_kinds(args) {
            Ok(f) => f,
            Err(e) => return (e, true),
        };

        // Resolve both sides with Aggregate policy.
        let from_resolution = match self.resolve_graph_symbol_with_focus_retry(
            &from_input,
            SymbolResolutionPolicy::Aggregate,
            None,
            Some(max_depth),
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };
        let to_resolution = match self.resolve_graph_symbol_with_focus_retry(
            &to_input,
            SymbolResolutionPolicy::Aggregate,
            None,
            Some(max_depth),
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        // Extract SymbolId lists from both resolutions.
        let from_ids: Vec<SymbolId> =
            match resolution_to_symbol_ids_and_meta(&from_resolution, from_qname) {
                Ok((ids, _)) => ids,
                Err(e) => {
                    if let Some(qname) = not_found_resolution_qname(&from_resolution) {
                        return self.retryable_symbol_not_found_response(
                            "path",
                            args,
                            qname,
                            Vec::new(),
                            Some("path requires the source symbol to be materialized first".into()),
                        );
                    }
                    return (e, true);
                }
            };

        let to_ids: Vec<SymbolId> =
            match resolution_to_symbol_ids_and_meta(&to_resolution, to_qname) {
                Ok((ids, _)) => ids,
                Err(e) => {
                    if let Some(qname) = not_found_resolution_qname(&to_resolution) {
                        let detail = self
                            .unresolved_call_target_hint(&from_ids, to_qname)
                            .unwrap_or_else(|| {
                                "path requires the target symbol to be materialized first".into()
                            });
                        return self.retryable_symbol_not_found_response(
                            "path",
                            args,
                            qname,
                            Vec::new(),
                            Some(detail),
                        );
                    }
                    if let Some(hint) = self.unresolved_call_target_hint(&from_ids, to_qname) {
                        return (format!("{e}.{hint}"), true);
                    }
                    return (e, true);
                }
            };

        // Update investigation with the first "from" symbol
        if let Some(&first_from) = from_ids.first() {
            self.update_investigation(InvestigationFocus::Symbol(first_from));
        }
        let lr = AnalysisEnvelope::new("path", args);

        // Transparent lazy structural: ensure both endpoint files have full
        // structural data before path finding. A cold focus project may lack
        // the intra-file call edges that BFS needs to discover a path.
        let (_roots, root_warnings) = self.include_roots_from_args(args);
        let intent = Some(atlas_engine::QueryIntent::Path {
            from_name: from_qname.to_string(),
            to_name: to_qname.to_string(),
            max_depth: Some(max_depth),
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);
        let lazy_warnings = focus_warnings;
        // Cache for no-path diagnostics below (used in user-facing messages).
        let is_manual_full = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };

        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let snap = graph.snapshot();

        // Try all SymbolId pairs for the same qname.  In C/C++, a symbol
        // declared in a header (.h) and defined in a source file (.c)
        // produces two SymbolIds sharing the same qualified name — only the
        // definition's ID has outgoing call edges.  The first pair (from_ids[0]
        // → to_ids[0]) matches the pre-fix behaviour; fallback pairs are
        // tried only when the first attempt fails.
        let mut ranked = Vec::new();
        let mut winning_from = None;
        let mut winning_to = None;
        'id_search: for fid in &from_ids {
            for tid in &to_ids {
                if from_qname == to_qname && fid == tid {
                    continue;
                }
                // K=5: find up to 5 alternative paths, ranked by composite score.
                // Convert SymbolId → NodeIx for the snapshot.
                let from_ix = match snap.id_to_idx.get(fid) {
                    Some(ix) => *ix,
                    None => continue,
                };
                let to_ix = match snap.id_to_idx.get(tid) {
                    Some(ix) => *ix,
                    None => continue,
                };
                let candidates = snap.k_ranked_paths(
                    from_ix,
                    to_ix,
                    5,
                    max_depth.min(10),
                    edge_kind_filter.as_deref(),
                    direction,
                    prefer_production,
                );
                if !candidates.is_empty() {
                    ranked = candidates;
                    winning_from = Some(*fid);
                    winning_to = Some(*tid);
                    break 'id_search;
                }
            }
        }

        /// Resolve a SymbolId to a compact "file:line" label for ambiguity
        /// reporting.
        fn symbol_label(store: &Store, id: &SymbolId) -> String {
            store
                .find_symbol_by_id(id)
                .ok()
                .flatten()
                .map(|s| {
                    format!(
                        "{}:{}",
                        store
                            .get_file(&s.file_id)
                            .ok()
                            .flatten()
                            .map(|f| f.path.clone())
                            .unwrap_or_else(|| s.file_id.to_hex()),
                        s.range.start_line + 1
                    )
                })
                .unwrap_or_else(|| id.to_hex())
        }

        /// Build JSON hops for a single path.
        fn build_hops(
            store_query: &crate::tools::runtime::store_query_runtime::StoreQueryRuntime,
            snap: &atlas_engine::GraphSnapshot,
            path: &atlas_engine::GraphPath,
            include_code: bool,
        ) -> Vec<serde_json::Value> {
            let mut hops: Vec<serde_json::Value> =
                Vec::with_capacity(path.node_indices.len() + path.edge_indices.len());
            for i in 0..path.node_indices.len() {
                let mut node_json = super::node_json(store_query, snap, path.node_indices[i], None);
                if include_code {
                    let node = snap.node(path.node_indices[i]);
                    if let Some(src) = store_query.read_symbol_source(&node.symbol_id) {
                        node_json["source"] = json!(src);
                    }
                }
                hops.push(node_json);
                if i < path.edges.len() {
                    let edge = snap.edge(path.edges[i].edge_ix);
                    hops.push(json!({
                        "edge_kind": edge.kind.as_str(),
                        "direction": path.edges[i].direction.as_str(),
                        "confidence": edge.confidence.as_f32(),
                    }));
                }
            }
            hops
        }

        if !ranked.is_empty() {
            let snap = graph.snapshot();

            // Primary path (rank 0) gets the full treatment.
            let primary = &ranked[0];
            let hops = build_hops(
                &self.project().store_query_runtime,
                snap,
                &primary.path,
                include_code,
            );
            let breakpoints: Vec<serde_json::Value> = primary.path.breakpoints.iter().map(|bp| {
                json!({ "kind": bp.kind.as_str(), "edge_index": bp.edge_index, "message": bp.message })
            }).collect();

            let mut resp = json!({
                "from": from_qname,
                "to": to_qname,
                "path_length": primary.path.node_indices.len(),
                "confidence": primary.path.confidence,
                "total_weight": primary.path.total_weight,
                "test_hops": primary.path.test_hops,
                "indirect_hops": primary.path.indirect_hops,
                "path": hops,
                "breakpoints": breakpoints,
                "score": {
                    "overall": primary.scores.overall,
                    "semantic": primary.scores.semantic,
                    "topology": primary.scores.topology,
                    "centrality": primary.scores.centrality,
                },
            });

            // Alternative paths (ranks 1+) — compact summaries.
            if ranked.len() > 1 {
                let alternatives: Vec<serde_json::Value> = ranked[1..]
                    .iter()
                    .map(|r| {
                        let alt_hops =
                            build_hops(&self.project().store_query_runtime, snap, &r.path, false);
                        json!({
                            "path": alt_hops,
                            "total_weight": r.path.total_weight,
                            "score": {
                                "overall": r.scores.overall,
                                "semantic": r.scores.semantic,
                                "topology": r.scores.topology,
                                "centrality": r.scores.centrality,
                            },
                        })
                    })
                    .collect();
                resp["alternatives"] = json!(alternatives);
            }

            // Ambiguity metadata — include resolution info
            if from_ids.len() > 1 || to_ids.len() > 1 {
                let mut ambiguity = json!({});
                if from_ids.len() > 1 {
                    if let Some(ref wid) = winning_from {
                        ambiguity["matched_from"] = json!(symbol_label(&self.project().store, wid));
                    }
                    ambiguity["from_count"] = json!(from_ids.len());
                    // Add from_candidates list (truncated to MAX_AMBIGUOUS_CANDIDATES)
                    let from_candidates: Vec<serde_json::Value> = from_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| {
                            candidate_json(
                                &self.project().store,
                                id,
                                Some(id) == winning_from.as_ref(),
                            )
                        })
                        .collect();
                    if !from_candidates.is_empty() {
                        ambiguity["from_candidates"] = json!(from_candidates);
                    }
                }
                if to_ids.len() > 1 {
                    if let Some(ref wid) = winning_to {
                        ambiguity["matched_to"] = json!(symbol_label(&self.project().store, wid));
                    }
                    ambiguity["to_count"] = json!(to_ids.len());
                    // Add to_candidates list (truncated to MAX_AMBIGUOUS_CANDIDATES)
                    let to_candidates: Vec<serde_json::Value> = to_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| {
                            candidate_json(
                                &self.project().store,
                                id,
                                Some(id) == winning_to.as_ref(),
                            )
                        })
                        .collect();
                    if !to_candidates.is_empty() {
                        ambiguity["to_candidates"] = json!(to_candidates);
                    }
                }
                // Add structured from_resolution metadata
                ambiguity["from_resolution"] = build_resolution_meta_for_path(&from_resolution);
                if to_ids.len() > 1 {
                    ambiguity["to_resolution"] = build_resolution_meta_for_path(&to_resolution);
                }
                // Add selection note
                if from_ids.len() > 1 || to_ids.len() > 1 {
                    ambiguity["selection_note"] =
                        json!("Selected first (from, to) pair with a discoverable path.");
                }
                resp["ambiguity"] = ambiguity;
            }

            // ── Path quality insight ───────────────────────────────────
            //
            // When the found path has low semantic quality (proxy/fallback
            // patterns, low centrality), the true primary path was likely
            // blocked by unresolved function pointers or dynamic dispatch.
            // Compute guidance on where annotations would help.

            let quality = if primary.scores.semantic >= 0.8 && primary.scores.overall >= 0.7 {
                "direct"
            } else if primary.scores.semantic >= 0.5 {
                "indirect"
            } else {
                "fallback"
            };

            let mut insight = json!({ "quality": quality });

            if quality == "fallback" || quality == "indirect" {
                // Find function-pointer registration sites reachable from
                // the path nodes — these are likely the reason the primary
                // path wasn't found.
                let mut fp_sites: Vec<serde_json::Value> = Vec::new();
                for &nix in &primary.path.node_indices {
                    let regs = snap.incoming_neighbors_with_kinds(&snap.node(nix).symbol_id);
                    for (reg_ix, ek) in &regs {
                        if *ek == atlas_engine::EdgeKind::RegistersCallback {
                            let reg_node = snap.node(*reg_ix);
                            fp_sites.push(json!({
                                "at": reg_node.qualified_name,
                                "registers": snap.node(nix).qualified_name,
                                "guidance": format!(
                                    "fp_dispatches(action=\"add\", field_qname='{}...', target_qname='{}')",
                                    reg_node.qualified_name, snap.node(nix).qualified_name
                                ),
                            }));
                        }
                    }
                    if fp_sites.len() >= 5 {
                        break;
                    }
                }
                if !fp_sites.is_empty() {
                    insight["fp_boundaries"] = json!(fp_sites);
                }

                // Compute forward frontier from source to show where
                // function-pointer boundaries exist.
                let frontier = snap.forward_frontier(
                    &[snap.node(primary.path.node_indices[0]).symbol_id],
                    max_depth.min(10),
                    edge_kind_filter.as_deref(),
                );
                let blocked: Vec<serde_json::Value> = frontier
                    .frontier_nodes
                    .iter()
                    .take(5)
                    .filter(|n| n.outgoing_call_count == 0)
                    .map(|n| json!({ "qname": n.qname, "depth": n.depth }))
                    .collect();
                if !blocked.is_empty() {
                    insight["blocked_at"] = json!({
                        "message": format!(
                            "{} node(s) with no static forward edges — likely function-pointer or dynamic-dispatch boundaries",
                            blocked.len()
                        ),
                        "nodes": blocked,
                    });
                }

                insight["action"] = json!(
                    "The primary path is likely blocked by unresolved function pointers. Use 'fp_dispatches' (action='add') to declare known dispatches (e.g., curl handler tables, vtable assignments), then re-run the path query after annotation materialization."
                );
            }

            resp["path_quality"] = insight;

            let lr = lr
                .with_root_warnings(root_warnings)
                .with_lazy_warnings(lazy_warnings);
            let lr = if let Some(ref result) = focus_result {
                crate::tools::apply_focus_result_to_lr(lr, result)
            } else {
                lr
            };
            lr.build(resp, self)
        } else {
            // No path found — diagnostic frontier.
            let total_pairs = from_ids.len() * to_ids.len();
            let mut no_path_warnings = lazy_warnings;
            let mut message = format!(
                "No path found within max_depth={} (tried {} SymbolId pair{})",
                max_depth.min(10),
                total_pairs,
                if total_pairs == 1 { "" } else { "s" },
            );
            // ... (same diagnostics as before) ...
            if total_pairs > 1 {
                if from_ids.len() > 10 || to_ids.len() > 10 {
                    message.push_str(&format!(
                        ". Note: '{}' matched {} SymbolId(s), '{}' matched {} SymbolId(s) — this is likely symbol-name ambiguity across files. Use a fully-qualified name to narrow the search.",
                        from_qname, from_ids.len(), to_qname, to_ids.len(),
                    ));
                } else {
                    message.push_str(". Note: the same qualified name maps to multiple SymbolIds (e.g., declaration vs definition). All pairs were tried.");
                }
            }
            if !is_manual_full && max_depth < 10 {
                message.push_str(". In focus mode this is only a current-closure result, not a repo-wide proof. Tip: try a higher max_depth (up to 10), resume the query after refinement, or run a full structural index (CLI: 'atlas index --analysis full') for deeper call-graph edges.");
            } else if !is_manual_full {
                message.push_str(". In focus mode this is only a current-closure result, not a repo-wide proof. Tip: the path may involve function pointers or dynamic dispatch not yet resolved; resume the query after refinement or run a full structural index (CLI: 'atlas index --analysis full').");
            } else {
                message.push_str(". The symbols may not be connected by call edges, or the path exceeds the depth limit. Try a higher max_depth.");
            }
            if !is_manual_full {
                no_path_warnings.push(
                    "No path was found in the current focus closure; this does not prove that no repo-wide path exists until full indexing or further refinement completes."
                        .to_string(),
                );
            }

            // Resolve endpoint symbol kinds for type-aware diagnostics.
            // Uses the first SymbolId per qname (most common case).
            let from_kind = from_ids
                .first()
                .and_then(|id| snap.node_by_id(id))
                .map(|n| n.kind);
            let to_kind = to_ids
                .first()
                .and_then(|id| snap.node_by_id(id))
                .map(|n| n.kind);
            if let (Some(fk), Some(tk)) = (from_kind, to_kind) {
                message.push_str(&format!(
                    " (from '{from_qname}' resolved as {fk:?}, to '{to_qname}' resolved as {tk:?})",
                ));
                if !is_callable_kind(tk) {
                    message.push_str(". Note: target is not a callable — specify a method or function instead (e.g. use the fully-qualified method name).");
                }
                if !is_callable_kind(fk) {
                    message.push_str(". Note: source is not a callable — outgoing call edges originate from functions/methods, not from type definitions.");
                }
            }

            const MAX_FRONTIER_NODES: usize = 20;
            let frontier_nodes: Vec<serde_json::Value> = if direction
                == TraversalDirection::Outgoing
            {
                let frontier = snap.forward_frontier(
                    &from_ids,
                    max_depth.min(10),
                    edge_kind_filter.as_deref(),
                );
                let total = frontier.frontier_nodes.len();
                if total > 0 {
                    let extra = if total > MAX_FRONTIER_NODES {
                        " These are likely dynamic-dispatch (function pointer / virtual call) boundaries."
                    } else {
                        ""
                    };
                    message.push_str(&format!(
                            "\nForward frontier reached depth {} — {} node(s) with no further static callees (showing first {}).{}",
                            frontier.depth_reached, total, total.min(MAX_FRONTIER_NODES), extra,
                        ));
                }
                frontier.frontier_nodes.iter().take(MAX_FRONTIER_NODES).map(|n| {
                        json!({ "qname": n.qname, "depth": n.depth, "outgoing_call_count": n.outgoing_call_count })
                    }).collect()
            } else {
                Vec::new()
            };
            // Build base response before envelope injection.
            let mut resp = json!({
                "from": from_qname, "to": to_qname,
                "path_length": 0, "path": [], "breakpoints": [],
                "message": &message, "frontier": frontier_nodes,
            });

            // Add candidates and disambiguation guidance when symbols are ambiguous.
            if from_ids.len() > 1 || to_ids.len() > 1 {
                if from_ids.len() > 1 {
                    let from_candidates: Vec<serde_json::Value> = from_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| candidate_json(&self.project().store, id, false))
                        .collect();
                    if !from_candidates.is_empty() {
                        resp["from_candidates"] = json!(from_candidates);
                    }
                }
                if to_ids.len() > 1 {
                    let to_candidates: Vec<serde_json::Value> = to_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| candidate_json(&self.project().store, id, false))
                        .collect();
                    if !to_candidates.is_empty() {
                        resp["to_candidates"] = json!(to_candidates);
                    }
                }
                resp["message"] = json!(format!(
                    "{message}\nUse a SymbolSelector object (for example, {{\"qualified_name\": \"...\", \"file_path\": \"...\"}}) to disambiguate; symbol_ref from search or symbol results can be reused directly."
                ));
            }

            let lr = lr
                .with_root_warnings(root_warnings)
                .with_lazy_warnings(no_path_warnings);
            let lr = if let Some(ref result) = focus_result {
                crate::tools::apply_focus_result_to_lr(lr, result)
            } else {
                lr
            };
            lr.build(resp, self)
        }
    }

    /// Resolve the `direction` parameter for path finding.
    /// - Not provided or "outgoing" → TraversalDirection::Outgoing (only forward edges)
    /// - "incoming" → TraversalDirection::Incoming (only reverse/caller edges)
    /// - "both" → TraversalDirection::Both (forward + reverse; use when tracing
    ///   who-calls-X-to-reach-Y scenarios or backward provenance)
    fn resolve_path_direction(args: &serde_json::Value) -> TraversalDirection {
        match get_str_opt(args, "direction") {
            Some("outgoing") => TraversalDirection::Outgoing,
            Some("incoming") => TraversalDirection::Incoming,
            Some("both") => TraversalDirection::Both,
            _ => TraversalDirection::Outgoing,
        }
    }

    /// Resolve the `edge_kinds` parameter to an optional edge kind filter.
    /// - Not provided → defaults to call edges only (DEFAULT_PATH_EDGES)
    /// - Empty array or `["*"]` → follows all edge kinds (None)
    /// - Specific kinds → filtered to those kinds
    fn resolve_path_edge_kinds(args: &serde_json::Value) -> Result<Option<Vec<EdgeKind>>, String> {
        let raw = match args.get("edge_kinds") {
            None | Some(serde_json::Value::Null) => {
                return Ok(Some(DEFAULT_PATH_EDGES.to_vec()));
            }
            Some(v) => v,
        };
        let arr = raw
            .as_array()
            .ok_or_else(|| "edge_kinds must be an array of strings".to_string())?;
        if arr.is_empty() {
            return Ok(None); // all edge kinds
        }
        if arr.len() == 1 && arr[0].as_str() == Some("*") {
            return Ok(None); // wildcard → all edge kinds
        }
        let mut kinds = Vec::with_capacity(arr.len());
        for v in arr {
            let s = v.as_str().unwrap_or("");
            if s == "*" {
                return Err("'*' must be the only value in edge_kinds".to_string());
            }
            kinds.push(parse_edge_kind(s)?);
        }
        Ok(Some(kinds))
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
            Some(
                self.prepare_focus_query(Some(atlas_engine::QueryIntent::Explore {
                    symbol_name: qname.to_string(),
                    file_id: self.resolve_selector_file_id(&input),
                    symbol_id: None,
                })),
            )
        } else {
            None
        };

        // Resolve with UniqueOrCandidates policy.
        let resolution = if let Some(scope) = scope {
            match self.scoped_explore_resolution(qname, scope) {
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

        let lr = AnalysisEnvelope::new("explore", args);

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
                    "status": "building",
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
            self.prepare_focus_query(Some(atlas_engine::QueryIntent::Explore {
                symbol_name: qname.to_string(),
                file_id: Some(seed_sym.file_id),
                symbol_id: None,
            }))
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

        // ── Semantic impact analysis ────────────────────────────────────
        let mut invariants: Vec<serde_json::Value> = Vec::new();
        let mut lifecycle_paths: Vec<serde_json::Value> = Vec::new();
        let domain_rules = if semantic {
            match self.project().store.list_domain_rules(None, None) {
                Ok(_rows) => {
                    let lang_str = self
                        .project()
                        .store
                        .find_symbol_by_id(&sid)
                        .ok()
                        .flatten()
                        .map(|s| s.language.as_str())
                        .unwrap_or("c");
                    Some(analysis::CppOwnershipRules::load_for(
                        &self.project().store,
                        lang_str,
                    ))
                }
                Err(e) => {
                    tracing::warn!("Failed to load domain rules: {e}");
                    None
                }
            }
        } else {
            None
        };

        if semantic {
            for &ix in sub.node_indices.iter().take(20) {
                let node = snap.node(ix);
                // Only analyze callable symbols
                if !is_callable_kind(node.kind) {
                    continue;
                }

                // Load CFG for this function
                let cfg_nodes = match self
                    .project()
                    .store
                    .find_cfg_nodes_by_function(&node.symbol_id)
                {
                    Ok(nodes) => nodes,
                    Err(_) => continue,
                };
                if cfg_nodes.is_empty() {
                    continue;
                }

                // Run branch diff analysis
                let cfg_edges = self
                    .project()
                    .store
                    .find_cfg_edges_by_function(&node.symbol_id)
                    .unwrap_or_default();
                // ── Semantic branch diff with dataflow composition ──
                let lang = self
                    .project()
                    .store
                    .find_symbol_by_id(&node.symbol_id)
                    .ok()
                    .flatten()
                    .map(|s| s.language)
                    .unwrap_or(atlas_engine::Language::C);
                let contract = atlas_engine::analysis::ResourceOpConfig::default_for(lang);

                // Load DataFlow nodes and edges
                let data_nodes = self
                    .project()
                    .store
                    .find_data_nodes_by_function(&node.symbol_id)
                    .unwrap_or_default();
                let dataflow_edges = if data_nodes.is_empty() {
                    vec![]
                } else {
                    let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
                    self.project()
                        .store
                        .find_dataflow_edges_by_sources(&all_ids)
                        .unwrap_or_default()
                };

                let composition = match atlas_engine::analysis::cfg_graph::CfgGraph::build(
                    &cfg_nodes, &cfg_edges,
                ) {
                    Ok(cfg_graph) => atlas_engine::analysis::compose_effects(
                        &cfg_graph,
                        &data_nodes,
                        &dataflow_edges,
                        &contract,
                    ),
                    Err(_) => atlas_engine::analysis::EffectComposition::default(),
                };

                let diffs = analysis::BranchDiffEngine::diff_branches_semantic(
                    &cfg_nodes,
                    &cfg_edges,
                    &composition,
                );

                // Collect fields that have effect annotations (both legacy and semantic)
                let mut fields: HashSet<String> = HashSet::new();
                for effects in composition.node_effects.values() {
                    for eff in effects {
                        use atlas_engine::effects::PlaceRef;
                        match &eff.kind {
                            atlas_engine::effects::SemanticEffectKind::Free {
                                place: PlaceRef::Field { path },
                                ..
                            }
                            | atlas_engine::effects::SemanticEffectKind::Alloc {
                                target: PlaceRef::Field { path },
                                ..
                            }
                            | atlas_engine::effects::SemanticEffectKind::Store {
                                dst: PlaceRef::Field { path },
                                ..
                            } => {
                                if !path.is_empty() {
                                    fields.insert(path.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // For each field, run lifecycle analysis
                for field_path in &fields {
                    let ownership_rules = analysis::OwnershipRules::default();
                    let cpp_rules = domain_rules
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(analysis::CppOwnershipRules::default);
                    let mut lifecycle = analysis::FieldLifecycleEngine::analyze_with_composition(
                        &cfg_nodes,
                        &cfg_edges,
                        field_path,
                        &ownership_rules,
                        &cpp_rules,
                        &composition,
                    );
                    lifecycle.function_qname = node.qualified_name.clone();

                    if !lifecycle.suspicious_points.is_empty() {
                        invariants.push(json!({
                            "function": node.qualified_name,
                            "field": field_path,
                            "issue_count": lifecycle.suspicious_points.len(),
                            "issues": lifecycle.suspicious_points.iter().map(|p| json!({
                                "line": p.line,
                                "kind": format!("{:?}", p.kind),
                                "message": p.message,
                            })).collect::<Vec<_>>(),
                        }));
                    }

                    if lifecycle.transitions.len() >= 2 {
                        lifecycle_paths.push(json!({
                            "function": node.qualified_name,
                            "field": field_path,
                            "final_state": lifecycle.final_state.as_str(),
                            "transition_count": lifecycle.transitions.len(),
                        }));
                    }
                }

                // Add branch diffs with asymmetry
                for diff in &diffs {
                    if let Some(ref asymmetry) = diff.suspicious_asymmetry {
                        invariants.push(json!({
                            "function": node.qualified_name,
                            "field": diff.common_prefix,
                            "issue_count": 1,
                            "issues": [{"kind": "BranchAsymmetry", "message": asymmetry, "line": diff.branch_node_line}],
                        }));
                    }
                }
            }
        }

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
        if semantic {
            resp["semantic_impact"] = json!({
                "invariants_affected": invariants,
                "lifecycle_paths_affected": lifecycle_paths,
                "domain_rules_applied": domain_rules.is_some(),
            });
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
mod tests {
    use super::EdgeKind;
    use super::parse_edge_kind;
    use super::resolution_to_symbol_ids_and_meta;
    use crate::tools::ToolRouter;
    use crate::tools::symbol_selector::SymbolResolution;
    use atlas_engine::Store;
    use atlas_engine::ids::FileId;
    use serde_json::json;
    use std::sync::Arc;

    // ── Helpers ─────────────────────────────────────────────────────────
    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    fn test_router(store: Arc<Store>) -> ToolRouter {
        ToolRouter::new_empty(store, std::path::PathBuf::from("/tmp/test_project"))
    }

    fn insert_test_symbol(store: &Store, path: &str, qname: &str) -> atlas_engine::SymbolId {
        let fid = FileId::generate(path);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: path.into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sid = atlas_engine::SymbolId::generate(&fid, "typescript", qname, "function", None);
        store
            .insert_symbols(&[atlas_engine::SymbolDef {
                id: sid,
                kind: atlas_engine::SymbolKind::Function,
                name: qname.rsplit('.').next().unwrap_or(qname).into(),
                qualified_name: qname.into(),
                symbol_path: qname.split('.').map(str::to_string).collect(),
                file_id: fid,
                language: atlas_engine::Language::TypeScript,
                range: atlas_engine::TextRange::default(),
                name_range: atlas_engine::TextRange::default(),
                signature: None,
                visibility: None,
                exported: false,
                static_: false,
                async_: false,
                container: None,
                scope_id: None,
                package_name: None,
                namespace_path: vec![],
                layer: "structural".into(),
            }])
            .unwrap();
        sid
    }

    #[test]
    fn graph_refresh_observes_external_store_changes_immediately() {
        let store = test_store();
        let _sid_a = insert_test_symbol(&store, "a.ts", "a");
        let router = test_router(store.clone());
        router.ensure_graph_initialized().unwrap();

        let sid_b = insert_test_symbol(&store, "b.ts", "b");
        let before = router
            .project()
            .graph_runtime
            .provider()
            .graph_snapshot()
            .unwrap()
            .impact_with_kinds(
                &sid_b,
                1,
                Some(vec![]),
                atlas_engine::TraversalDirection::Outgoing,
            )
            .node_indices
            .len();
        assert_eq!(before, 0, "precondition: old graph should not contain b");

        // Bump graph_generation to signal that the store has changed (external
        // store mutation that bypasses the overlay runtime). This replaces the
        // old TTL + signature-check cooldown bypass.
        router
            .project()
            .graph_runtime
            .invalidation
            .graph_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        router.maybe_refresh_graph().unwrap();
        let after = router
            .project()
            .graph_runtime
            .provider()
            .graph_snapshot()
            .unwrap()
            .impact_with_kinds(
                &sid_b,
                1,
                Some(vec![]),
                atlas_engine::TraversalDirection::Outgoing,
            )
            .node_indices
            .len();
        assert_eq!(after, 1, "refreshed graph should contain b");
    }

    #[test]
    fn test_parse_edge_kind_all_valid() {
        let cases: &[(&str, EdgeKind)] = &[
            ("calls", EdgeKind::Calls),
            ("instantiates", EdgeKind::Instantiates),
            ("implements", EdgeKind::Implements),
            ("registers_callback", EdgeKind::RegistersCallback),
            ("references", EdgeKind::References),
            ("contains", EdgeKind::Contains),
            ("imports", EdgeKind::Imports),
            ("includes", EdgeKind::Includes),
            ("exports", EdgeKind::Exports),
            ("extends", EdgeKind::Extends),
            ("typeof", EdgeKind::TypeOf),
            ("returns", EdgeKind::Returns),
            ("overrides", EdgeKind::Overrides),
            ("decorates", EdgeKind::Decorates),
            ("defines", EdgeKind::Defines),
            ("argument", EdgeKind::Argument),
            ("parameter", EdgeKind::Parameter),
            ("assigns", EdgeKind::Assigns),
            ("reads", EdgeKind::Reads),
            ("writes", EdgeKind::Writes),
            ("field_read", EdgeKind::FieldRead),
            ("field_write", EdgeKind::FieldWrite),
        ];
        for (s, expected) in cases {
            let result = parse_edge_kind(s);
            assert!(result.is_ok(), "Expected Ok for '{s}', got Err");
            assert_eq!(result.unwrap(), *expected, "Wrong EdgeKind for '{s}'");
        }
    }

    #[test]
    fn test_parse_edge_kind_invalid() {
        let cases = &["", "*", "unknown_edge", "Calls", "calls "];
        for s in cases {
            let result = parse_edge_kind(s);
            assert!(result.is_err(), "Expected Err for '{s}', got Ok");
        }
    }

    #[test]
    fn test_parse_edge_kind_imports() {
        assert_eq!(parse_edge_kind("imports"), Ok(EdgeKind::Imports));
        assert_eq!(parse_edge_kind("includes"), Ok(EdgeKind::Includes));
    }

    #[test]
    fn test_parse_edge_kind_instantiates() {
        assert_eq!(parse_edge_kind("instantiates"), Ok(EdgeKind::Instantiates));
        assert_eq!(
            parse_edge_kind("registers_callback"),
            Ok(EdgeKind::RegistersCallback)
        );
    }

    // ── handle_impact argument parsing and error paths ──────────────────

    #[test]
    fn test_handle_impact_missing_symbol_argument() {
        let store = test_store();
        // Register a file so the store isn't completely empty
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({}));
        assert!(is_error, "expected error for missing symbol, got: {resp}");
    }

    #[test]
    fn test_handle_impact_invalid_edge_kind_string() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "anything",
            "edge_kinds": ["nonexistent_edge"]
        }));
        assert!(
            is_error,
            "expected error for invalid edge kind, got: {resp}"
        );
        // Verify the error message mentions the invalid kind
        let resp_lower = resp.to_lowercase();
        assert!(
            resp_lower.contains("unknown edge kind") || resp_lower.contains("nonexistent"),
            "error should mention invalid edge kind, got: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_mixed_wildcard_returns_error() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "anything",
            "edge_kinds": ["*", "calls"]
        }));
        assert!(is_error, "expected error for mixed wildcard, got: {resp}");
        assert!(
            resp.contains("must be the only value"),
            "error message mismatch: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_edge_kinds_not_array() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "anything",
            "edge_kinds": "calls"
        }));
        assert!(
            is_error,
            "expected error for non-array edge_kinds, got: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_direction_defaults_to_outgoing() {
        let store = test_store();
        let router = test_router(store);
        // Symbol won't exist, but argument parsing happens before resolve_qname
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent"
        }));
        // In focus mode, symbol-not-found returns a partial "building" result
        // (is_error=false) instead of a hard error. Both acceptable.
        assert!(
            is_error || resp.contains("building") || resp.contains("not available"),
            "nonexistent symbol should error or return partial: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_accepts_outgoing_direction() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent",
            "direction": "outgoing"
        }));
        // In focus mode, symbol-not-found returns a partial "building" result
        // (is_error=false) instead of a hard error. Both are acceptable.
        assert!(
            is_error || resp.contains("building") || resp.contains("not available"),
            "nonexistent symbol should error or return partial: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_accepts_incoming_direction() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent",
            "direction": "incoming"
        }));
        assert!(
            is_error || resp.contains("building") || resp.contains("not available"),
            "nonexistent symbol should error or return partial: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_accepts_both_direction() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent",
            "direction": "both"
        }));
        assert!(
            is_error || resp.contains("building") || resp.contains("not available"),
            "nonexistent symbol should error or return partial: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_with_direction_param() {
        // Verify that direction="both" is accepted and processed without error
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "test_func",
            "direction": "both",
            "depth": 2
        }));
        // direction="both" must not produce an argument parsing error.
        // Symbol may not exist (focus mode returns partial), but that's ok.
        assert!(
            is_error || resp.contains("building") || resp.contains("not available"),
            "direction='both' should be accepted; got: {resp}"
        );
    }

    #[test]
    fn test_handle_impact_invalid_direction_returns_error() {
        let store = test_store();
        let router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({
            "symbol": "anything",
            "direction": "sideways"
        }));
        assert!(
            is_error,
            "expected error for invalid direction, got: {resp}"
        );
        assert!(
            resp.contains("direction must be"),
            "error should mention valid directions, got: {resp}"
        );
    }

    // ── lazy structural response fields ─────────────────────────────────

    #[test]
    fn test_handle_impact_response_has_warnings_field() {
        let store = test_store();
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sym = atlas_engine::SymbolDef {
            id: atlas_engine::SymbolId::generate(&fid, "typescript", "main", "function", None),
            kind: atlas_engine::SymbolKind::Function,
            name: "main".into(),
            qualified_name: "main".into(),
            symbol_path: vec!["main".into()],
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_impact(&json!({"symbol": "main"}));
        assert!(!is_error, "expected success, got error: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        // FocusRuntime is always present (created in QueryRuntime::new).
        // The "FocusRuntime not initialized" warning/error path has been
        // removed. Warnings may still be present for other reasons
        // (focus gaps, etc.).
        let warnings = resp.get("warnings");
        if let Some(w) = warnings {
            assert!(w.is_array(), "warnings should be an array");
            // The removed "FocusRuntime not initialized" warning must NOT appear.
            let arr = w.as_array().unwrap();
            for entry in arr {
                if let Some(s) = entry.as_str() {
                    assert!(
                        !s.contains("FocusRuntime not initialized"),
                        "FocusRuntime initialization warning should not appear, got: {s}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_handle_impact_response_has_direction() {
        let store = test_store();
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sym = atlas_engine::SymbolDef {
            id: atlas_engine::SymbolId::generate(&fid, "typescript", "f", "function", None),
            kind: atlas_engine::SymbolKind::Function,
            name: "f".into(),
            qualified_name: "f".into(),
            symbol_path: vec!["f".into()],
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) =
            router.handle_impact(&json!({"symbol": "f", "direction": "both"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["direction"], "both");
    }

    #[test]
    fn test_handle_callees_response_omits_internal_precision() {
        let store = test_store();
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sym = atlas_engine::SymbolDef {
            id: atlas_engine::SymbolId::generate(&fid, "typescript", "g", "function", None),
            kind: atlas_engine::SymbolKind::Function,
            name: "g".into(),
            qualified_name: "g".into(),
            symbol_path: vec!["g".into()],
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_callees(&json!({"symbol": "g"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert!(resp.get("precision").is_none());
    }

    #[test]
    fn test_handle_callers_response_omits_internal_precision() {
        let store = test_store();
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sym = atlas_engine::SymbolDef {
            id: atlas_engine::SymbolId::generate(&fid, "typescript", "h", "function", None),
            kind: atlas_engine::SymbolKind::Function,
            name: "h".into(),
            qualified_name: "h".into(),
            symbol_path: vec!["h".into()],
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_callers(&json!({"symbol": "h"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert!(resp.get("precision").is_none());
    }

    /// Verify `handle_callers` deduplicates when aggregating multiple
    /// SymbolIds (e.g. same qname in different files). A shared caller of
    /// all matched targets must appear exactly once in the results.
    #[test]
    fn test_aggregate_dedup_calls() {
        let store = test_store();

        // Two "target" symbols: same qname, different files → ambiguous.
        let target_a = insert_test_symbol(&store, "src/a.ts", "target");
        let target_b = insert_test_symbol(&store, "src/b.ts", "target");

        // shared_caller calls BOTH target symbols
        let shared_caller = insert_test_symbol(&store, "src/caller.ts", "shared_caller");
        insert_test_call_edge(&store, shared_caller, target_a);
        insert_test_call_edge(&store, shared_caller, target_b);

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        let (resp_str, is_error) = router.handle_callers(&json!({"symbol": "target"}));
        assert!(!is_error, "expected success, got error: {resp_str}");

        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");

        let total = resp["total_callers"]
            .as_u64()
            .expect("should have total_callers");
        let callers = resp["callers"]
            .as_array()
            .expect("should have callers array");

        // Dedup: shared_caller must appear exactly once
        assert_eq!(
            total, 1,
            "total_callers should be 1 after dedup, got {total}"
        );
        assert_eq!(
            callers.len(),
            1,
            "callers array should have 1 entry after dedup, got {} entries: {callers:?}",
            callers.len()
        );

        let caller = &callers[0];
        assert_eq!(
            caller["qualified_name"].as_str().unwrap(),
            "shared_caller",
            "caller should be shared_caller"
        );
        assert!(
            caller["file"].as_str().unwrap().contains("caller.ts"),
            "caller file should be caller.ts"
        );
    }

    // ── handle_explore tests ───────────────────────────────────────────

    #[test]
    fn explore_unique_symbol_returns_dossier() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "test.ts", "myfunc");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "myfunc"}));
        assert!(!is_error, "expected success, got error: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert!(
            resp.get("subject").is_some(),
            "response should have 'subject' field, got keys: {:?}",
            resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
        assert!(
            resp.get("precisionTier").is_none(),
            "explore must not expose internal precision tier: {resp}"
        );
    }

    #[test]
    fn explore_ambiguous_symbol_returns_list() {
        let store = test_store();
        insert_test_symbol(&store, "a.ts", "shared_func");
        insert_test_symbol(&store, "b.ts", "shared_func");
        insert_test_symbol(&store, "c.ts", "shared_func");
        let router = test_router(store);
        // Ambiguous path returns early; graph init not needed
        let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "shared_func"}));
        assert!(!is_error, "expected not an error, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["ambiguous"], json!(true), "should be ambiguous");
        let candidates = resp
            .get("candidates")
            .and_then(|v| v.as_array())
            .expect("should have candidates array");
        assert_eq!(candidates.len(), 3, "expected 3 candidates");
    }

    #[test]
    fn explore_accepts_source_lines_param() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "test.ts", "myfunc2");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) =
            router.handle_explore(&json!({"symbol": "myfunc2", "source_lines": 10}));
        assert!(
            !is_error,
            "expected no error with source_lines param, got: {resp_str}"
        );
        // Params are accepted if handler does not reject them.
        // Dossier may still build with warnings about missing source files.
    }

    #[test]
    fn explore_accepts_evidence_limit_param() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "test.ts", "myfunc3");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) =
            router.handle_explore(&json!({"symbol": "myfunc3", "evidence_limit": 5}));
        assert!(
            !is_error,
            "expected no error with evidence_limit param, got: {resp_str}"
        );
    }

    #[test]
    fn explore_not_found_returns_retryable_partial_response() {
        let store = test_store();
        let router = test_router(store);
        let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "missing_func"}));
        assert!(
            !is_error,
            "missing cold symbol should be a retryable partial response: {resp_str}"
        );
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["status"], json!("building"));
        assert_eq!(resp["analysis"]["retry_after_ms"], json!(2000));
        assert!(resp.get("background_refinement").is_none());
    }

    // ── ambiguity candidate tests ──────────────────────────────────────

    #[test]
    fn max_ambiguous_candidates_is_5() {
        // Verify the constant was changed
        assert_eq!(super::super::MAX_AMBIGUOUS_CANDIDATES, 5);
    }

    #[test]
    fn ambiguity_includes_candidates_when_multiple() {
        let store = test_store();

        // Insert two symbols with same qname but different files
        let sid1 = insert_test_symbol(&store, "src/a.ts", "ns.foo");
        let sid2 = insert_test_symbol(&store, "src/b.ts", "ns.foo");

        // Build candidate JSON manually to test helper
        let c = super::candidate_json(&store, &sid1, true);
        assert_eq!(c["qualified_name"], "ns.foo");
        assert_eq!(c["selected"], true);
        assert!(c["file"].as_str().unwrap().contains("a.ts"));

        // Second candidate not selected
        let c2 = super::candidate_json(&store, &sid2, false);
        assert_eq!(c2["qualified_name"], "ns.foo");
        assert_eq!(c2["selected"], false);
        assert!(c2["file"].as_str().unwrap().contains("b.ts"));
    }

    // ── path multi-candidate-pair exhaustive search ────────────────────

    /// Helper: insert a call edge between two symbols in the store.
    fn insert_test_call_edge(
        store: &Store,
        source: atlas_engine::SymbolId,
        target: atlas_engine::SymbolId,
    ) {
        let edge = atlas_engine::RawEdge::new(
            atlas_engine::EdgeId::generate(&source, &target, "calls", None, "tree_sitter"),
            source,
            target,
            atlas_engine::EdgeKind::Calls,
            atlas_engine::Confidence::new(1.0),
            atlas_engine::Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();
    }

    fn insert_unresolved_call_reference(store: &Store, source: atlas_engine::SymbolId, name: &str) {
        let source_symbol = store
            .find_symbol_by_id(&source)
            .unwrap()
            .expect("source symbol should exist");
        let range = atlas_engine::TextRange {
            start_byte: 32,
            end_byte: 32 + name.len() as u32,
            start_line: 4,
            start_column: 8,
            end_line: 4,
            end_column: 8 + name.len() as u32,
        };
        let reference = atlas_engine::ReferenceUse {
            id: atlas_engine::ReferenceId::generate(
                &source_symbol.file_id,
                Some(&source),
                range.start_byte,
                range.end_byte,
                name,
                atlas_engine::ReferenceKind::Call,
            ),
            file_id: source_symbol.file_id,
            source_symbol: Some(source),
            scope_id: None,
            kind: atlas_engine::ReferenceKind::Call,
            text: name.to_string(),
            name: name.to_string(),
            receiver: None,
            arity: Some(1),
            range,
            binding_id: None,
            resolved: None,
        };
        store.insert_references(&[reference]).unwrap();
    }

    /// Verify that handle_path with ambiguous string qnames tries all
    /// SymbolId pairs and returns resolution + candidate metadata.
    ///
    /// Scenario: 2 "from" candidates × 2 "to" candidates = 4 pairs total.
    /// Only 1 of the 4 pairs has a call edge → the first winning pair is
    /// selected and returned.
    #[test]
    fn test_path_multi_pair_exhaustive() {
        let store = test_store();

        // 2 "from" candidates: same qname "sender", different files
        let from_sid0 = insert_test_symbol(&store, "src/a.ts", "sender");
        let _from_sid1 = insert_test_symbol(&store, "src/b.ts", "sender");

        // 2 "to" candidates: same qname "receiver", different files
        let to_sid0 = insert_test_symbol(&store, "src/c.ts", "receiver");
        let _to_sid1 = insert_test_symbol(&store, "src/d.ts", "receiver");

        // Only 1 of 4 pairs has a path: from_sid0 → to_sid0
        insert_test_call_edge(&store, from_sid0, to_sid0);

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        // Plain string qnames (not SymbolSelectors)
        let (resp_str, is_error) = router.handle_path(&json!({
            "from": "sender",
            "to": "receiver"
        }));
        assert!(!is_error, "expected success, got error: {resp_str}");

        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");

        // A non-empty path was found
        assert!(
            resp.get("path")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false),
            "should have a non-empty path, got: {resp_str}"
        );

        // ── Ambiguity metadata ──────────────────────────────────────────
        let ambiguity = resp.get("ambiguity").expect("should have ambiguity field");

        assert_eq!(
            ambiguity["from_count"].as_u64().unwrap(),
            2,
            "from_count should be 2"
        );
        assert_eq!(
            ambiguity["to_count"].as_u64().unwrap(),
            2,
            "to_count should be 2"
        );

        // from_candidates / to_candidates
        let from_cands = ambiguity["from_candidates"]
            .as_array()
            .expect("from_candidates should be an array");
        assert_eq!(from_cands.len(), 2, "from_candidates should have 2 entries");

        let to_cands = ambiguity["to_candidates"]
            .as_array()
            .expect("to_candidates should be an array");
        assert_eq!(to_cands.len(), 2, "to_candidates should have 2 entries");

        // from_resolution / to_resolution count
        let from_res = ambiguity
            .get("from_resolution")
            .expect("should have from_resolution");
        assert_eq!(
            from_res["count"].as_u64().unwrap(),
            2,
            "from_resolution count should be 2"
        );

        let to_res = ambiguity
            .get("to_resolution")
            .expect("should have to_resolution");
        assert_eq!(
            to_res["count"].as_u64().unwrap(),
            2,
            "to_resolution count should be 2"
        );

        // Winning pair markers
        let from_selected = from_cands
            .iter()
            .any(|c| c.get("selected").and_then(|v| v.as_bool()).unwrap_or(false));
        assert!(
            from_selected,
            "at least one from_candidate should be selected"
        );

        let to_selected = to_cands
            .iter()
            .any(|c| c.get("selected").and_then(|v| v.as_bool()).unwrap_or(false));
        assert!(to_selected, "at least one to_candidate should be selected");

        assert!(
            ambiguity.get("matched_from").is_some(),
            "should have matched_from"
        );
        assert!(
            ambiguity.get("matched_to").is_some(),
            "should have matched_to"
        );

        // selection_note confirms pair-based search
        assert!(
            ambiguity
                .get("selection_note")
                .and_then(|v| v.as_str())
                .map(|s| s.contains("pair"))
                .unwrap_or(false),
            "selection_note should mention pair-based selection"
        );
    }

    #[test]
    fn path_ambiguous_no_path_uses_message_without_hint_field() {
        let store = test_store();
        insert_test_symbol(&store, "src/a.ts", "sender");
        insert_test_symbol(&store, "src/b.ts", "sender");
        insert_test_symbol(&store, "src/c.ts", "receiver");
        insert_test_symbol(&store, "src/d.ts", "receiver");

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (response, is_error) = router.handle_path(&json!({
            "from": "sender",
            "to": "receiver"
        }));
        assert!(!is_error, "expected bounded no-path response: {response}");

        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(response.get("hint").is_none(), "{response}");
        assert!(
            response["message"]
                .as_str()
                .is_some_and(|message| message.contains("SymbolSelector")),
            "disambiguation guidance should remain in message: {response}"
        );
    }

    #[test]
    fn path_not_found_target_reports_unresolved_call_hint() {
        let store = test_store();
        let from_id = insert_test_symbol(&store, "src/a.ts", "sender");
        insert_unresolved_call_reference(&store, from_id, "copy_from_user");

        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        let (resp, is_error) = router.handle_path(&json!({
            "from": "sender",
            "to": "copy_from_user"
        }));

        // In focus mode, symbol-not-found returns a partial "building" result
        // (is_error=false) instead of a hard error. Both acceptable outcomes.
        if is_error {
            assert!(
                resp.contains("unresolved call token")
                    && resp.contains("calls(direction=\"outgoing\")"),
                "missing actionable unresolved-call hint: {resp}"
            );
        } else {
            assert!(
                resp.contains("building")
                    || resp.contains("not available")
                    || resp.contains("partial"),
                "focus-mode partial result should indicate building state: {resp}"
            );
        }
    }

    // ── resolution_to_symbol_ids_and_meta unit tests ──────────────────

    #[test]
    fn resolution_helper_single_returns_id_and_meta() {
        use atlas_engine::symbol_selector::{
            MatchInfo, MatchMode, PathMatchQuality, ResolvedSymbol,
        };
        let sid = atlas_engine::SymbolId::from_bytes([1u8; 32]);
        let resolved = ResolvedSymbol {
            qualified_name: "foo".into(),
            file_path: "src/lib.rs".into(),
            line: 10,
            kind: "function".into(),
            language: "rust".into(),
            match_info: MatchInfo {
                mode: MatchMode::UniqueQname,
                ignored_mismatches: vec![],
                path_match: Some(PathMatchQuality::Exact),
                line_delta: None,
            },
        };
        let resolution = SymbolResolution::Single {
            symbol_id: sid,
            resolved,
        };
        let (ids, meta) = resolution_to_symbol_ids_and_meta(&resolution, "foo").unwrap();
        assert_eq!(ids, vec![sid]);
        assert!(meta.is_some());
    }

    #[test]
    fn resolution_helper_ambiguous_uses_direct_symbol_id() {
        use atlas_engine::symbol_selector::{ScoredCandidate, SymbolSelector};
        let sid1 = atlas_engine::SymbolId::from_bytes([1u8; 32]);
        let sid2 = atlas_engine::SymbolId::from_bytes([2u8; 32]);
        let candidates = vec![
            ScoredCandidate {
                qualified_name: "foo".into(),
                file_path: "a.rs".into(),
                line: 10,
                kind: "function".into(),
                language: "rust".into(),
                score: 100,
                reasons: vec![],
                symbol_ref: SymbolSelector {
                    qualified_name: "foo".into(),
                    file_path: Some("a.rs".into()),
                    line: Some(10),
                    kind: Some("function".into()),
                    language: Some("rust".into()),
                },
                symbol_id: sid1,
            },
            ScoredCandidate {
                qualified_name: "foo".into(),
                file_path: "b.rs".into(),
                line: 20,
                kind: "function".into(),
                language: "rust".into(),
                score: 80,
                reasons: vec![],
                symbol_ref: SymbolSelector {
                    qualified_name: "foo".into(),
                    file_path: Some("b.rs".into()),
                    line: Some(20),
                    kind: Some("function".into()),
                    language: Some("rust".into()),
                },
                symbol_id: sid2,
            },
        ];
        let resolution = SymbolResolution::Ambiguous {
            candidates,
            score_gap: 20,
        };
        let (ids, meta) = resolution_to_symbol_ids_and_meta(&resolution, "foo").unwrap();
        assert_eq!(ids, vec![sid1, sid2]);
        assert!(meta.is_some());
    }

    #[test]
    fn resolution_helper_not_found_returns_error() {
        let resolution = SymbolResolution::NotFound {
            qname: "missing_fn".into(),
            suggestions: vec!["other_fn".into()],
        };
        let err = resolution_to_symbol_ids_and_meta(&resolution, "missing_fn").unwrap_err();
        assert!(err.contains("not found"));
        assert!(err.contains("other_fn"));
    }

    #[test]
    fn resolution_to_symbol_ids_uses_direct_symbol_id() {
        // Verify that when ScoredCandidate carries symbol_id, we don't need
        // find_symbols_by_qname round-trip.
        use atlas_engine::symbol_selector::{ScoredCandidate, SymbolSelector};
        let sid1 = atlas_engine::SymbolId::from_bytes([1u8; 32]);
        let sid2 = atlas_engine::SymbolId::from_bytes([2u8; 32]);
        let candidates = vec![
            ScoredCandidate {
                qualified_name: "test_fn".into(),
                file_path: "src/a.rs".into(),
                line: 10,
                kind: "function".into(),
                language: "rust".into(),
                score: 100,
                reasons: vec![],
                symbol_ref: SymbolSelector {
                    qualified_name: "test_fn".into(),
                    file_path: Some("src/a.rs".into()),
                    line: Some(10),
                    kind: Some("function".into()),
                    language: Some("rust".into()),
                },
                symbol_id: sid1,
            },
            ScoredCandidate {
                qualified_name: "test_fn".into(),
                file_path: "src/b.rs".into(),
                line: 20,
                kind: "function".into(),
                language: "rust".into(),
                score: 80,
                reasons: vec![],
                symbol_ref: SymbolSelector {
                    qualified_name: "test_fn".into(),
                    file_path: Some("src/b.rs".into()),
                    line: Some(20),
                    kind: Some("function".into()),
                    language: Some("rust".into()),
                },
                symbol_id: sid2,
            },
        ];
        let ids: Vec<_> = candidates.iter().map(|c| c.symbol_id).collect();
        assert_eq!(ids, vec![sid1, sid2]);
    }

    // ── calls dispatch tests ──────────────────────────────────────────

    #[test]
    fn calls_dispatch_wildcard_edge_kinds_routes_to_callgraph() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "a.ts", "a.a");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        // Explicit empty edge_kinds [] means wildcard → should route to callgraph
        let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
            "symbol": "a.a",
            "direction": "outgoing",
            "edge_kinds": [],
        }));
        assert!(
            matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
            "wildcard edge_kinds should route to CallGraph"
        );
    }

    #[test]
    fn calls_dispatch_custom_edge_kinds_routes_to_callgraph() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "a.ts", "a.a");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        // Custom edge_kinds → should route to callgraph
        let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
            "symbol": "a.a",
            "direction": "outgoing",
            "edge_kinds": ["calls", "references"],
        }));
        assert!(
            matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
            "custom edge_kinds should route to CallGraph"
        );
    }

    #[test]
    fn calls_dispatch_default_edges_routes_to_specific_handler() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "a.ts", "a.a");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
            "symbol": "a.a",
            "direction": "incoming",
        }));
        assert!(
            matches!(dispatch, crate::tools::CallsDispatch::Callers),
            "incoming with default edges should route to Callers"
        );
    }

    #[test]
    fn calls_dispatch_both_direction_routes_to_callgraph() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "a.ts", "a.a");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
            "symbol": "a.a",
            "direction": "both",
        }));
        assert!(
            matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
            "'both' direction should route to CallGraph"
        );
    }

    #[test]
    fn calls_dispatch_depth_gt_1_routes_to_callgraph() {
        let store = test_store();
        let _sid = insert_test_symbol(&store, "a.ts", "a.a");
        let router = test_router(store);
        router.ensure_graph_initialized().unwrap();

        let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
            "symbol": "a.a",
            "direction": "outgoing",
            "depth": 3,
        }));
        assert!(
            matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
            "depth > 1 should route to CallGraph"
        );
    }

    #[test]
    fn calls_dispatch_unknown_direction_returns_error() {
        let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
            "symbol": "a.a",
            "direction": "sideways",
        }));
        assert!(
            matches!(dispatch, crate::tools::CallsDispatch::Error(_)),
            "unknown direction should return Error"
        );
    }

    // ── Focus runtime wiring tests ────────────────────────────────────

    #[test]
    fn init_focus_sets_focus_runtime() {
        let store = test_store();
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let router = test_router(store);
        // After construction, focus_runtime is always present (no Option wrapper).
        let mode = router.project().query_runtime.detect_index_mode();
        assert_eq!(mode, atlas_engine::focus::runtime::IndexMode::Focus);
        // init_focus is idempotent.
        router.init_focus();
        let mode2 = router.project().query_runtime.detect_index_mode();
        assert_eq!(mode2, atlas_engine::focus::runtime::IndexMode::Focus);
    }

    #[test]
    fn graph_response_without_focus_has_no_focus_fields() {
        let store = test_store();
        let fid = FileId::generate("test.ts");
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sym = atlas_engine::SymbolDef {
            id: atlas_engine::SymbolId::generate(
                &fid,
                "typescript",
                "focus_test_fn",
                "function",
                None,
            ),
            kind: atlas_engine::SymbolKind::Function,
            name: "focus_test_fn".into(),
            qualified_name: "focus_test_fn".into(),
            symbol_path: vec!["focus_test_fn".into()],
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();

        let router = test_router(store);
        // Simulate a full index so prepare_focus_query returns early
        // without focus data — the equivalent of the old "no focus" path.
        let signature = router.project().store.index_signature().unwrap_or_default();
        *router
            .project()
            .query_runtime
            .cache
            .cached_manual_full_index
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some((signature, true));
        router.ensure_graph_initialized().unwrap();
        // focus is NOT active for this query — focus fields should NOT appear
        let (resp_str, is_error) = router.handle_impact(&json!({"symbol": "focus_test_fn"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");

        // Backward compat: focus-specific fields must NOT appear when focus is not active
        assert!(
            resp.get("coverage_counts").is_none(),
            "coverage_counts should NOT appear when focus is not active"
        );
        assert!(
            resp.get("gaps").is_none(),
            "gaps should NOT appear when focus is not active"
        );
        assert!(
            resp.get("pending_closures").is_none(),
            "pending_closures should NOT appear when focus is not active"
        );
    }

    #[test]
    fn apply_focus_to_lr_is_noop_with_no_focus_data() {
        use crate::tools::analysis_envelope::AnalysisEnvelope;
        use atlas_engine::focus::runtime::{FocusResult, IndexMode};

        let result = FocusResult {
            mode: IndexMode::FullIndex,
            precision: None,
            gaps: vec![],
            pending_closure_ids: vec![],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: None,
        };

        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("test", &args).with_is_error(false);
        let lr = crate::tools::apply_focus_result_to_lr(lr, &result);

        // Build with a mock store to verify no crash
        let store = MockStore::new();
        let body = json!({"ok": true});
        let (json_str, is_err) = lr.build(body, &store);
        assert!(!is_err, "should succeed with no focus data");
        // No focus fields should be injected
        assert!(
            !json_str.contains("coverage_counts"),
            "coverage_counts should not be present when focus data is None"
        );
        assert!(
            !json_str.contains("gaps"),
            "gaps should not be present when focus data is None"
        );
        assert!(
            !json_str.contains("pending_closures"),
            "pending_closures should not be present when focus data is None"
        );
    }

    // Mock SnapshotStore for isolated AnalysisEnvelope tests
    use std::sync::Mutex;

    struct MockStore {
        snapshots: Mutex<Vec<crate::tools::query_snapshot::QuerySnapshot>>,
    }
    impl MockStore {
        fn new() -> Self {
            Self {
                snapshots: Mutex::new(Vec::new()),
            }
        }
    }
    impl crate::tools::analysis_envelope::SnapshotStore for MockStore {
        fn store_query_snapshot(&self, snapshot: crate::tools::query_snapshot::QuerySnapshot) {
            self.snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(snapshot);
        }
    }
}
