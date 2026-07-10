//! Call graph handlers: fixed one-hop callers/callees and bounded BFS expansion.

use std::collections::HashSet;

use atlas_engine::{EdgeKind, InvestigationFocus, SymbolId};
use serde_json::json;

use super::{
    not_found_resolution_qname, parse_edge_kind, parse_symbol_arg,
    resolution_to_symbol_ids_and_meta, symbol_input_qname,
};
use crate::tools::analysis_envelope::AnalysisEnvelope;
use crate::tools::symbol_selector::SymbolResolutionPolicy;
use crate::tools::{ToolRouter, get_str, get_u64};

/// Check whether an edge kind is allowed by a configurable filter.
/// An empty `allowed` slice means *all* edge kinds are allowed.
fn is_allowed_edge(kind: &EdgeKind, allowed: &[EdgeKind]) -> bool {
    if allowed.is_empty() {
        return true; // wildcard / all edges
    }
    allowed.contains(kind)
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
            "signature": sym.signature,
        }))
    }

    pub(crate) fn handle_callers(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = crate::tools::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;
        let (include_roots, mut tool_warnings) = self.include_roots_from_args(args);
        // callers/callees are fixed 1-hop; multi-hop is callgraph / direction=both+depth.
        // (tool_warnings: include_roots validation + call-depth policy; not roots-only.)
        if args.get("depth").is_some() {
            tool_warnings.push(
                "depth is not honored for calls(direction=incoming); \
                 use direction=both with depth, or the callgraph tool, for multi-hop"
                    .into(),
            );
        }

        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::Aggregate,
            Some("incoming".to_string()),
            None,
            &include_roots,
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
        let lr = AnalysisEnvelope::new("calls", args).with_root_warnings(tool_warnings);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: self.resolve_selector_file_id(&input),
            symbol_id: Some(sid),
            direction: Some("incoming".to_string()),
            depth: None, // fixed 1-hop regardless of client depth arg
        });
        let (focus_result, mut focus_warnings) =
            self.prepare_focus_query_with_roots(intent, include_roots);

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
                    all_callers.push(crate::tools::node_json(
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
        if let Err(e) = crate::tools::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;
        let (include_roots, mut tool_warnings) = self.include_roots_from_args(args);
        // tool_warnings: include_roots validation + call-depth policy; not roots-only.
        if args.get("depth").is_some() {
            tool_warnings.push(
                "depth is not honored for calls(direction=outgoing); \
                 use direction=both with depth, or the callgraph tool, for multi-hop"
                    .into(),
            );
        }

        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::Aggregate,
            Some("outgoing".to_string()),
            None,
            &include_roots,
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
        let lr = AnalysisEnvelope::new("calls", args).with_root_warnings(tool_warnings);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: self.resolve_selector_file_id(&input),
            symbol_id: Some(sid),
            direction: Some("outgoing".to_string()),
            depth: None, // fixed 1-hop regardless of client depth arg
        });
        let (focus_result, mut focus_warnings) =
            self.prepare_focus_query_with_roots(intent, include_roots);

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
                    all_callees.push(crate::tools::node_json(
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
        if let Err(e) = crate::tools::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;
        let limit = get_u64(args, "limit").unwrap_or(100) as usize;

        let direction = get_str(args, "direction");
        let edge_kinds = match resolve_call_edge_kinds(args) {
            Ok(k) => k,
            Err(e) => return (e, true),
        };
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

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
            &include_roots,
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
        let lr = AnalysisEnvelope::new("calls", args).with_root_warnings(root_warnings);

        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: qname.to_string(),
            file_id: self.resolve_selector_file_id(&input),
            symbol_id: Some(sid),
            direction: retry_direction,
            depth: Some(depth),
        });
        let (focus_result, lazy_warnings) =
            self.prepare_focus_query_with_roots(intent, include_roots);

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
                root_nodes.push(crate::tools::node_json(
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
}
