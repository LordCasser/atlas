//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use std::collections::HashSet;

use atlas_engine::analysis;
use atlas_engine::{EdgeKind, InvestigationFocus, Store, SymbolId, SymbolKind, TraversalDirection};
use atlas_engine::dossier::SourceRepository;

use super::lazy_response::LazyResponse;
use super::{MAX_AMBIGUOUS_CANDIDATES, MAX_SYMBOL_NAME_LENGTH, ToolRouter, get_str, get_str_opt, get_u64};
use crate::tools::symbol_selector::{
    ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
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
        SymbolResolution::Single { symbol_id, resolved } => {
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
    pub(crate) fn handle_callers(&mut self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let resolution = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::Aggregate) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) =
            match resolution_to_symbol_ids_and_meta(&resolution, qname) {
                Ok(r) => r,
                Err(e) => return (e, true),
            };

        let sid = symbol_ids[0];
        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = LazyResponse::new("calls", args);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: None,
            symbol_id: None,
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);

        let cb = match self.context_builder() {
            Ok(cb) => cb,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let graph = cb.graph_snapshot();
        let snap = graph.snapshot();

        // Multi-root: union of callers from all matched SymbolIds, deduplicated.
        let mut seen: HashSet<SymbolId> = HashSet::new();
        let mut all_callers: Vec<atlas_engine::NodeIx> = Vec::new();
        for &root_id in &symbol_ids {
            let cg = graph.callers(&root_id);
            for &ix in &cg.callers {
                let caller_id = snap.node(ix).symbol_id;
                if seen.insert(caller_id) {
                    all_callers.push(ix);
                }
            }
        }
        let total_callers = all_callers.len();
        let shown = all_callers.iter().take(limit);
        let nodes: Vec<_> = shown.map(|ix| self.node_json(snap, *ix, None)).collect();

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
        if !self.active.query_runtime.cache.has_manual_full_index(&self.active.store) {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }

        // Inject graph edge provenance when operating in FocusPartial mode.
        self.inject_graph_precision(&mut resp);

        // Lazy structural response with focus-aware envelope
        let lr = lr.with_lazy_warnings(focus_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp, args, self)
    }

    pub(crate) fn handle_callees(&mut self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let resolution = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::Aggregate) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) =
            match resolution_to_symbol_ids_and_meta(&resolution, qname) {
                Ok(r) => r,
                Err(e) => return (e, true),
            };

        let sid = symbol_ids[0];
        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = LazyResponse::new("calls", args);

        // Lazy structural: prepare focus query for graph edges
        let mut file_ids_set: HashSet<atlas_engine::FileId> = HashSet::new();
        for &id in &symbol_ids {
            if let Some(sym) = self.active.store.find_symbol_by_id(&id).ok().flatten() {
                file_ids_set.insert(sym.file_id);
            }
        }
        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: file_ids_set.iter().next().copied(),
            symbol_id: None,
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);

        let cb = match self.context_builder() {
            Ok(cb) => cb,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let graph = cb.graph_snapshot();
        let snap = graph.snapshot();

        // Multi-root: union of callees from all matched SymbolIds, deduplicated.
        let mut seen: HashSet<SymbolId> = HashSet::new();
        let mut all_callees: Vec<atlas_engine::NodeIx> = Vec::new();
        for &root_id in &symbol_ids {
            let cg = graph.callees(&root_id);
            for &ix in &cg.callees {
                let callee_id = snap.node(ix).symbol_id;
                if seen.insert(callee_id) {
                    all_callees.push(ix);
                }
            }
        }
        let total_callees = all_callees.len();
        let shown = all_callees.iter().take(limit);
        let nodes: Vec<_> = shown.map(|ix| self.node_json(snap, *ix, None)).collect();

        let mut resp = json!({
            "symbol": qname,
            "total_callees": total_callees,
            "callees": nodes,
        });
        if let Some(rm) = resolution_meta_opt {
            if symbol_ids.len() > 1 {
                resp["resolution"] = rm;
            }
        }
        if !self.active.query_runtime.cache.has_manual_full_index(&self.active.store) {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }

        // Inject graph edge provenance when operating in FocusPartial mode.
        self.inject_graph_precision(&mut resp);

        // Lazy structural response with focus-aware envelope
        let lr = lr.with_lazy_warnings(focus_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp, args, self)
    }

    pub(crate) fn handle_callgraph(&mut self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;
        let limit = get_u64(args, "limit").unwrap_or(100) as usize;

        let direction = get_str(args, "direction");
        let edge_kinds = match resolve_call_edge_kinds(args) {
            Ok(k) => k,
            Err(e) => return (e, true),
        };

        let resolution = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::Aggregate) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) =
            match resolution_to_symbol_ids_and_meta(&resolution, qname) {
                Ok(r) => r,
                Err(e) => return (e, true),
            };

        let sid = symbol_ids[0];
        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = LazyResponse::new("calls", args);

        // Lazy structural: ensure graph edges exist before querying.
        let mut file_ids_set: HashSet<atlas_engine::FileId> = HashSet::new();
        for &id in &symbol_ids {
            if let Some(sym) = self.active.store.find_symbol_by_id(&id).ok().flatten() {
                file_ids_set.insert(sym.file_id);
            }
        }
        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: file_ids_set.iter().next().copied(),
            symbol_id: None,
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);
        let lazy_warnings = focus_warnings;

        let cb = match self.context_builder() {
            Ok(cb) => cb,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let graph = cb.graph_snapshot();
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
                root_nodes.push(self.node_json(snap, ix, None));
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
                            "file": self.resolve_file_path(&snap.node(neighbor_ix).file_id),
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
                            "file": self.resolve_file_path(&snap.node(neighbor_ix).file_id),
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
        if !self.active.query_runtime.cache.has_manual_full_index(&self.active.store) {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }

        // Inject graph edge provenance when operating in FocusPartial mode.
        self.inject_graph_precision(&mut resp);

        let lr = lr.with_lazy_warnings(lazy_warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp, args, self)
    }

    pub(crate) fn handle_path(&mut self, args: &serde_json::Value) -> (String, bool) {
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
        if from_qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("from exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }
        if to_qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("to exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
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
        let from_resolution = match self.resolve_symbol_input(&from_input, SymbolResolutionPolicy::Aggregate) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };
        let to_resolution = match self.resolve_symbol_input(&to_input, SymbolResolutionPolicy::Aggregate) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        // Extract SymbolId lists from both resolutions.
        let from_ids: Vec<SymbolId> =
            match resolution_to_symbol_ids_and_meta(&from_resolution, from_qname) {
                Ok((ids, _)) => ids,
                Err(e) => return (e, true),
            };

        let to_ids: Vec<SymbolId> =
            match resolution_to_symbol_ids_and_meta(&to_resolution, to_qname) {
                Ok((ids, _)) => ids,
                Err(e) => return (e, true),
            };

        // Update investigation with the first "from" symbol
        if let Some(&first_from) = from_ids.first() {
            self.update_investigation(InvestigationFocus::Symbol(first_from));
        }
        let lr = LazyResponse::new("path", args);

        // Transparent lazy structural: ensure both endpoint files have full
        // structural data before path finding.  A manifest-only index (MCP
        // default) may lack the intra-file call edges that BFS needs to
        // discover a path.
        let (_roots, root_warnings) = self.include_roots_from_args(args);
        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut file_ids_set: HashSet<atlas_engine::FileId> = HashSet::new();
        for id in from_ids.iter().chain(to_ids.iter()) {
            if let Some(sym) = self.active.store.find_symbol_by_id(id).ok().flatten() {
                file_ids_set.insert(sym.file_id);
            }
        }
        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: from_qname.to_string(),
            file_id: file_ids_set.iter().next().copied(),
            symbol_id: None,
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);
        let lazy_warnings = focus_warnings;
        // Cache for no-path diagnostics below (used in user-facing messages).
        let is_manual_full = self.active.query_runtime.cache.has_manual_full_index(&self.active.store);

        let cb = match self.context_builder() {
            Ok(cb) => cb,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let graph = cb.graph_snapshot();
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
            tool: &ToolRouter,
            snap: &atlas_engine::GraphSnapshot,
            path: &atlas_engine::GraphPath,
            include_code: bool,
        ) -> Vec<serde_json::Value> {
            let mut hops: Vec<serde_json::Value> =
                Vec::with_capacity(path.node_indices.len() + path.edge_indices.len());
            for i in 0..path.node_indices.len() {
                let mut node_json = tool.node_json(snap, path.node_indices[i], None);
                if include_code {
                    let node = snap.node(path.node_indices[i]);
                    if let Some(src) = tool.read_symbol_source(&node.symbol_id) {
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
            let hops = build_hops(self, snap, &primary.path, include_code);
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
                        let alt_hops = build_hops(self, snap, &r.path, false);
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
                        ambiguity["matched_from"] = json!(symbol_label(&self.active.store, wid));
                    }
                    ambiguity["from_count"] = json!(from_ids.len());
                    // Add from_candidates list (truncated to MAX_AMBIGUOUS_CANDIDATES)
                    let from_candidates: Vec<serde_json::Value> = from_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| {
                            candidate_json(
                                &self.active.store,
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
                        ambiguity["matched_to"] = json!(symbol_label(&self.active.store, wid));
                    }
                    ambiguity["to_count"] = json!(to_ids.len());
                    // Add to_candidates list (truncated to MAX_AMBIGUOUS_CANDIDATES)
                    let to_candidates: Vec<serde_json::Value> = to_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| {
                            candidate_json(
                                &self.active.store,
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
                    ambiguity["selection_note"] = json!(
                        "Selected first (from, to) pair with a discoverable path."
                    );
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

            // Inject graph edge provenance when operating in FocusPartial mode.
            self.inject_graph_precision(&mut resp);

            let lr = lr.with_root_warnings(root_warnings)
                .with_lazy_warnings(lazy_warnings)
                .with_partial_result(false);
            let lr = if let Some(ref result) = focus_result {
                crate::tools::apply_focus_result_to_lr(lr, result)
            } else {
                lr
            };
            lr.build(resp, self)
        } else {
            // No path found — diagnostic frontier.
            let total_pairs = from_ids.len() * to_ids.len();
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
                message.push_str(". Tip: try a higher max_depth (up to 10), or run a full structural index (CLI: 'atlas index' without --analysis manifest) for deeper call-graph edges.");
            } else if !is_manual_full {
                message.push_str(". Tip: the path may involve function pointers or dynamic dispatch not yet resolved. Try running a full structural index (CLI: 'atlas index').");
            } else {
                message.push_str(". The symbols may not be connected by call edges, or the path exceeds the depth limit. Try a higher max_depth.");
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

            // Add candidates and hint when symbols are ambiguous
            if from_ids.len() > 1 || to_ids.len() > 1 {
                if from_ids.len() > 1 {
                    let from_candidates: Vec<serde_json::Value> = from_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| candidate_json(&self.active.store, id, false))
                        .collect();
                    if !from_candidates.is_empty() {
                        resp["from_candidates"] = json!(from_candidates);
                    }
                }
                if to_ids.len() > 1 {
                    let to_candidates: Vec<serde_json::Value> = to_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| candidate_json(&self.active.store, id, false))
                        .collect();
                    if !to_candidates.is_empty() {
                        resp["to_candidates"] = json!(to_candidates);
                    }
                }
                resp["hint"] = json!("Use a SymbolSelector object (e.g. {\"qualified_name\": \"...\", \"file_path\": \"...\"}) to disambiguate. symbol_ref from search/symbol results can be reused directly.");
            }

            // Inject graph edge provenance when operating in FocusPartial mode.
            self.inject_graph_precision(&mut resp);

            let lr = lr.with_root_warnings(root_warnings)
                .with_lazy_warnings(lazy_warnings)
                .with_partial_result(false);
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

    pub(crate) fn handle_explore(&mut self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
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
            .unwrap_or(20) as usize;
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

        // Resolve with UniqueOrCandidates policy.
        let resolution = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::UniqueOrCandidates) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let lr = LazyResponse::new("explore", args);

        let (sym_id, resolved_opt) = match resolution {
            SymbolResolution::Single { symbol_id, resolved } => {
                (symbol_id, Some(resolved))
            }
            SymbolResolution::Ambiguous { candidates, score_gap } => {
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
            SymbolResolution::NotFound { ref qname, ref suggestions } => {
                let mut err = format!("Symbol not found: {qname}");
                if !suggestions.is_empty() {
                    err.push_str(&format!(". Did you mean: {}?", suggestions.join(", ")));
                }
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };

        let sym = match self.active.store.find_symbol_by_id(&sym_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let mut err = format!("Symbol not found in store: {qname}");
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
            Err(e) => {
                let mut err = format!("Lookup error: {e}");
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };

        let file_path = self.resolve_file_path(&sym.file_id);

        self.update_investigation(InvestigationFocus::Symbol(sym.id));

        // Lazy structural: prepare focus query for graph edges
        let intent = Some(atlas_engine::QueryIntent::Explore {
            symbol_name: qname.to_string(),
            file_id: Some(sym.file_id),
            symbol_id: None,
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);

        let sym_repo = atlas_engine::dossier::SymbolRepo::new(self.active.store.clone());
        let source_repo = atlas_engine::dossier::SourceRepo::new(
            self.active.store.clone(),
            self.active.root.clone(),
        );
        let cb = match self.context_builder() {
            Ok(cb) => cb,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let relation_repo = atlas_engine::dossier::RelationRepo::new(
            self.active.store.clone(),
            cb.graph_snapshot(),
        );
        let file_repo = atlas_engine::dossier::FileFactsRepo::new(self.active.store.clone());

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

        let mut resp_value = serde_json::to_value(&dossier)
            .unwrap_or_else(|e| json!({"error": e.to_string()}));

        // Include resolution metadata when resolved
        if let Some(ref resolved) = resolved_opt {
            resp_value["resolution"] = json!({
                "policy": "unique_or_candidates",
                "resolved": resolved,
            });
        }

        // Inject graph edge provenance when operating in FocusPartial mode.
        self.inject_graph_precision(&mut resp_value);

        let lr = lr.with_lazy_warnings(dossier.warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp_value, args, self)
    }

    pub(crate) fn handle_impact(&mut self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
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

        let resolution = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::Aggregate) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        let (symbol_ids, resolution_meta_opt) =
            match resolution_to_symbol_ids_and_meta(&resolution, qname) {
                Ok(r) => r,
                Err(e) => return (e, true),
            };

        let sid = symbol_ids[0];

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let lr = LazyResponse::new("impact", args);

        // Lazy structural: prepare focus query for impact analysis
        let file_id = self
            .active.store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.file_id);
        let intent = Some(atlas_engine::QueryIntent::Explore {
            symbol_name: qname.to_string(),
            file_id,
            symbol_id: None,
        });
        let (focus_result, focus_warnings) = self.prepare_focus_query(intent);

        let cb = match self.context_builder() {
            Ok(cb) => cb,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let graph = cb.graph_snapshot();
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
                    "file": self.resolve_file_path(&fid),
                    "symbols": symbols,
                })
            })
            .collect();

        // ── Semantic impact analysis ────────────────────────────────────
        let mut invariants: Vec<serde_json::Value> = Vec::new();
        let mut lifecycle_paths: Vec<serde_json::Value> = Vec::new();
        let domain_rules = if semantic {
            match self.active.store.list_domain_rules(None, None) {
                Ok(_rows) => {
                    let lang_str = self
                        .active.store
                        .find_symbol_by_id(&sid)
                        .ok()
                        .flatten()
                        .map(|s| s.language.as_str())
                        .unwrap_or("c");
                    Some(analysis::CppOwnershipRules::load_for(&self.active.store, lang_str))
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
                let cfg_nodes = match self.active.store.find_cfg_nodes_by_function(&node.symbol_id) {
                    Ok(nodes) => nodes,
                    Err(_) => continue,
                };
                if cfg_nodes.is_empty() {
                    continue;
                }

                // Run branch diff analysis
                let cfg_edges = self
                    .active.store
                    .find_cfg_edges_by_function(&node.symbol_id)
                    .unwrap_or_default();
                // ── Semantic branch diff with dataflow composition ──
                let lang = self
                    .active.store
                    .find_symbol_by_id(&node.symbol_id)
                    .ok()
                    .flatten()
                    .map(|s| s.language)
                    .unwrap_or(atlas_engine::Language::C);
                let contract = atlas_engine::analysis::ResourceOpConfig::default_for(lang);

                // Load DataFlow nodes and edges
                let data_nodes = self
                    .active.store
                    .find_data_nodes_by_function(&node.symbol_id)
                    .unwrap_or_default();
                let dataflow_edges = if data_nodes.is_empty() {
                    vec![]
                } else {
                    let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
                    self.active.store
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
                for n in &cfg_nodes {
                    // Semantic effects (preferred)
                    for eff in &n.semantic_effects {
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
                    let mut lifecycle = analysis::FieldLifecycleEngine::analyze_with_rules(
                        &cfg_nodes,
                        &cfg_edges,
                        field_path,
                        &ownership_rules,
                        &cpp_rules,
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
            "partial_result": truncated,
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
        if !self.active.query_runtime.cache.has_manual_full_index(&self.active.store) {
            resp["capability_note"] = json!(
                "manifest-only: structural data incomplete. Run 'atlas index' for full results."
            );
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
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

        // Inject graph edge provenance when operating in FocusPartial mode.
        self.inject_graph_precision(&mut resp);

        let lr = lr.with_root_warnings(Vec::new())
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
        let mut router = test_router(store.clone());
        router.ensure_graph_initialized().unwrap();

        let sid_b = insert_test_symbol(&store, "b.ts", "b");
        let before = router
            .context_builder()
            .unwrap()
            .graph_snapshot()
            .impact_with_kinds(
                &sid_b,
                1,
                Some(vec![]),
                atlas_engine::TraversalDirection::Outgoing,
            )
            .node_indices
            .len();
        assert_eq!(before, 0, "precondition: old graph should not contain b");

        router.maybe_refresh_graph().unwrap();
        let after = router
            .context_builder()
            .unwrap()
            .graph_snapshot()
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
        let mut router = test_router(store);
        let (resp, is_error) = router.handle_impact(&json!({}));
        assert!(is_error, "expected error for missing symbol, got: {resp}");
    }

    #[test]
    fn test_handle_impact_invalid_edge_kind_string() {
        let store = test_store();
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
        // Symbol won't exist, but argument parsing happens before resolve_qname
        let (_resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent"
        }));
        // Should error due to missing symbol, NOT due to missing direction
        assert!(is_error);
    }

    #[test]
    fn test_handle_impact_accepts_outgoing_direction() {
        let store = test_store();
        let mut router = test_router(store);
        let (_resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent",
            "direction": "outgoing"
        }));
        assert!(is_error); // still errors on missing symbol, but param accepted
    }

    #[test]
    fn test_handle_impact_accepts_incoming_direction() {
        let store = test_store();
        let mut router = test_router(store);
        let (_resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent",
            "direction": "incoming"
        }));
        assert!(is_error);
    }

    #[test]
    fn test_handle_impact_accepts_both_direction() {
        let store = test_store();
        let mut router = test_router(store);
        let (_resp, is_error) = router.handle_impact(&json!({
            "symbol": "nonexistent",
            "direction": "both"
        }));
        assert!(is_error);
    }

    #[test]
    fn test_handle_impact_invalid_direction_returns_error() {
        let store = test_store();
        let mut router = test_router(store);
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

        let mut router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_impact(&json!({"symbol": "main"}));
        assert!(!is_error, "expected success, got error: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        // After the fix (#2 HIGH), FocusRuntime is always initialized by the
        // constructor and activate_project.  The "No full index and FocusRuntime
        // not initialized" warning no longer appears.  Warnings may still be
        // present for other reasons (focus gaps, etc.), but they are not
        // guaranteed to be non-empty.
        let warnings = resp.get("warnings");
        if let Some(w) = warnings {
            assert!(w.is_array(), "warnings should be an array");
            // The "FocusRuntime not initialized" warning must NOT appear.
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

        let mut router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) =
            router.handle_impact(&json!({"symbol": "f", "direction": "both"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["direction"], "both");
    }

    #[test]
    fn test_handle_callees_response_has_warnings_and_precision() {
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

        let mut router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_callees(&json!({"symbol": "g"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    }

    #[test]
    fn test_handle_callers_response_has_warnings_and_precision() {
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

        let mut router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_callers(&json!({"symbol": "h"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
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

        let mut router = test_router(store);
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
        let mut router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "myfunc"}));
        assert!(!is_error, "expected success, got error: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert!(
            resp.get("subject").is_some(),
            "response should have 'subject' field, got keys: {:?}",
            resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }

    #[test]
    fn explore_ambiguous_symbol_returns_list() {
        let store = test_store();
        insert_test_symbol(&store, "a.ts", "shared_func");
        insert_test_symbol(&store, "b.ts", "shared_func");
        insert_test_symbol(&store, "c.ts", "shared_func");
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
        router.ensure_graph_initialized().unwrap();
        let (resp_str, is_error) =
            router.handle_explore(&json!({"symbol": "myfunc3", "evidence_limit": 5}));
        assert!(
            !is_error,
            "expected no error with evidence_limit param, got: {resp_str}"
        );
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

        let mut router = test_router(store);
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
        assert_eq!(
            from_cands.len(),
            2,
            "from_candidates should have 2 entries"
        );

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

    // ── resolution_to_symbol_ids_and_meta unit tests ──────────────────

    #[test]
    fn resolution_helper_single_returns_id_and_meta() {
        use atlas_engine::symbol_selector::{MatchInfo, MatchMode, PathMatchQuality, ResolvedSymbol};
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
        let resolution = SymbolResolution::Single { symbol_id: sid, resolved };
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
                line: 10, kind: "function".into(), language: "rust".into(),
                score: 100, reasons: vec![],
                symbol_ref: SymbolSelector {
                    qualified_name: "foo".into(),
                    file_path: Some("a.rs".into()),
                    line: Some(10), kind: Some("function".into()), language: Some("rust".into()),
                },
                symbol_id: sid1,
            },
            ScoredCandidate {
                qualified_name: "foo".into(),
                file_path: "b.rs".into(),
                line: 20, kind: "function".into(), language: "rust".into(),
                score: 80, reasons: vec![],
                symbol_ref: SymbolSelector {
                    qualified_name: "foo".into(),
                    file_path: Some("b.rs".into()),
                    line: Some(20), kind: Some("function".into()), language: Some("rust".into()),
                },
                symbol_id: sid2,
            },
        ];
        let resolution = SymbolResolution::Ambiguous { candidates, score_gap: 20 };
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
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
        let mut router = test_router(store);
        // After construction via new_empty, focus_runtime is already initialized.
        assert!(
            router.active.query_runtime.focus_runtime.is_some(),
            "focus_runtime should be Some after new_empty (init_focus called in constructor)"
        );
        // init_focus is idempotent.
        router.init_focus();
        assert!(
            router.active.query_runtime.focus_runtime.is_some(),
            "focus_runtime should remain Some after init_focus"
        );
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

        let mut router = test_router(store);
        // Explicitly clear focus_runtime to test the "without focus" code path.
        // After fix #2 HIGH, new_empty() initializes focus_runtime by default;
        // this test validates that the focus-less path still works correctly.
        router.active.query_runtime.focus_runtime = None;
        router.ensure_graph_initialized().unwrap();
        // focus_runtime is NOT initialized — focus fields should NOT appear
        let (resp_str, is_error) =
            router.handle_impact(&json!({"symbol": "focus_test_fn"}));
        assert!(!is_error, "expected success, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");

        // Backward compat: focus-specific fields must NOT appear when focus is not active
        assert!(
            resp.get("coverage_counts").is_none(),
            "coverage_counts should NOT appear when focus is not initialized"
        );
        assert!(
            resp.get("gaps").is_none(),
            "gaps should NOT appear when focus is not initialized"
        );
        assert!(
            resp.get("pending_closures").is_none(),
            "pending_closures should NOT appear when focus is not initialized"
        );
    }

    #[test]
    fn apply_focus_to_lr_is_noop_with_no_focus_data() {
        use atlas_engine::focus::runtime::{FocusResult, IndexMode};
        use crate::tools::lazy_response::LazyResponse;

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
        };

        let args = json!({"symbol": "test"});
        let lr = LazyResponse::new("test", &args)
            .with_is_error(false);
        let lr = crate::tools::apply_focus_result_to_lr(lr, &result);

        // Build with a mock store to verify no crash
        let mut store = MockStore::new();
        let body = json!({"ok": true});
        let (json_str, is_err) = lr.build(body, &mut store);
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

    // Mock SnapshotStore for isolated LazyResponse tests
    struct MockStore {
        snapshots: Vec<crate::tools::query_snapshot::QuerySnapshot>,
    }
    impl MockStore {
        fn new() -> Self {
            Self {
                snapshots: Vec::new(),
            }
        }
    }
    impl crate::tools::lazy_response::SnapshotStore for MockStore {
        fn store_query_snapshot(
            &mut self,
            snapshot: crate::tools::query_snapshot::QuerySnapshot,
        ) {
            self.snapshots.push(snapshot);
        }
    }
}
