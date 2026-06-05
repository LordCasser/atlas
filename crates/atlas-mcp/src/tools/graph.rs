//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use std::collections::HashSet;

use atlas_engine::analysis;
use atlas_engine::{EdgeKind, InvestigationFocus, Store, SymbolId, SymbolKind, TraversalDirection};

use super::lazy_response::{LazyDiagnostics, LazyResponse};
use super::{MAX_SYMBOL_NAME_LENGTH, ToolRouter, get_str, get_str_opt, get_u64};

use serde_json::json;

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

/// Check if a SymbolKind represents a callable entity (can appear as the
/// source or target of a Calls edge).
fn is_callable_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
    )
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
        let qname = get_str(args, "symbol");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
                true,
            );
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("calls", args);
        let query_id = lr.query_id().to_string();

        // Lazy structural: ensure graph edges exist before querying
        let file_id = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.file_id);
        let file_ids: Vec<atlas_engine::FileId> = file_id.into_iter().collect();
        let outcome_files = self.ensure_structural_for_files(
            file_ids,
            vec![],
            investigation.as_ref(),
            Some(&query_id),
        );
        let outcome_name = self.ensure_structural_for_symbol_name(
            qname,
            vec![],
            investigation.as_ref(),
            Some(&query_id),
        );

        let graph = self.context_builder().graph_snapshot();
        let cg = graph.callers(&sid);
        let snap = graph.snapshot();
        let shown = cg.callers.iter().take(limit);

        let nodes: Vec<_> = shown.map(|ix| self.node_json(snap, *ix, None)).collect();

        let mut resp = json!({
            "symbol": qname,
            "total_callers": cg.callers.len(),
            "callers": nodes,
        });
        if !self.has_manual_full_index() {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }
        // Lazy structural response — merge warnings from both outcomes
        let mut lazy_warnings: Vec<String> = outcome_files.warnings;
        lazy_warnings.extend(outcome_name.warnings);
        let tier = std::cmp::min(outcome_files.precision_tier, outcome_name.precision_tier);
        let lazy_diag: Option<LazyDiagnostics> = outcome_files
            .lazy_outcome
            .as_ref()
            .map(LazyDiagnostics::from_structural);

        lr.with_precision_tier(tier)
            .with_lazy_warnings(lazy_warnings)
            .with_lazy_diag(lazy_diag)
            .build_with_args(resp, args, self)
    }

    pub(crate) fn handle_callees(&mut self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
                true,
            );
        }
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("calls", args);
        let query_id = lr.query_id().to_string();

        // Lazy structural: ensure graph edges exist before querying
        let file_id = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.file_id);
        let file_ids: Vec<atlas_engine::FileId> = file_id.into_iter().collect();
        let outcome = self.ensure_structural_for_files(
            file_ids,
            vec![],
            investigation.as_ref(),
            Some(&query_id),
        );
        if let Err(e) = self.maybe_refresh_graph() {
            return (
                format!("Failed to refresh graph after structural ensure: {e:#}"),
                true,
            );
        }

        let graph = self.context_builder().graph_snapshot();
        let cg = graph.callees(&sid);
        let snap = graph.snapshot();
        let shown = cg.callees.iter().take(limit);

        let nodes: Vec<_> = shown.map(|ix| self.node_json(snap, *ix, None)).collect();

        let mut resp = json!({
            "symbol": qname,
            "total_callees": cg.callees.len(),
            "callees": nodes,
        });
        if !self.has_manual_full_index() {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }
        // Lazy structural response
        let lazy_warnings = outcome.warnings;
        let tier = outcome.precision_tier;
        let lazy_diag: Option<LazyDiagnostics> = outcome
            .lazy_outcome
            .as_ref()
            .map(LazyDiagnostics::from_structural);

        lr.with_precision_tier(tier)
            .with_lazy_warnings(lazy_warnings)
            .with_lazy_diag(lazy_diag)
            .build_with_args(resp, args, self)
    }

    pub(crate) fn handle_callgraph(&mut self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
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

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("calls", args);
        let query_id = lr.query_id().to_string();

        // Lazy structural: ensure graph edges exist before querying.
        // Direction-dependent: outgoing needs file edges, incoming needs
        // symbol-name candidate edges to find callers.
        let file_id = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.file_id);
        let file_ids: Vec<atlas_engine::FileId> = file_id.into_iter().collect();
        let (lazy_warnings, tier, lazy_outcome) = match direction {
            "incoming" => {
                let f = self.ensure_structural_for_files(
                    file_ids,
                    vec![],
                    investigation.as_ref(),
                    Some(&query_id),
                );
                let n = self.ensure_structural_for_symbol_name(
                    qname,
                    vec![],
                    investigation.as_ref(),
                    Some(&query_id),
                );
                let mut w = f.warnings;
                w.extend(n.warnings);
                let tier = std::cmp::min(f.precision_tier, n.precision_tier);
                (w, tier, f.lazy_outcome)
            }
            "outgoing" => {
                let f = self.ensure_structural_for_files(
                    file_ids,
                    vec![],
                    investigation.as_ref(),
                    Some(&query_id),
                );
                (f.warnings, f.precision_tier, f.lazy_outcome)
            }
            // "both" or default — need both directions
            _ => {
                let f = self.ensure_structural_for_files(
                    file_ids,
                    vec![],
                    investigation.as_ref(),
                    Some(&query_id),
                );
                let n = self.ensure_structural_for_symbol_name(
                    qname,
                    vec![],
                    investigation.as_ref(),
                    Some(&query_id),
                );
                let mut w = f.warnings;
                w.extend(n.warnings);
                let tier = std::cmp::min(f.precision_tier, n.precision_tier);
                (w, tier, f.lazy_outcome)
            }
        };

        let graph = self.context_builder().graph_snapshot();
        let snap = graph.snapshot();

        // Build hop-by-hop view: separate callers from callees at each depth.
        // This replaces the flat-list "both directions" output which confused
        // Agents by mixing callers and callees indiscriminately.
        let mut hops: Vec<serde_json::Value> = Vec::new();
        let mut total_nodes = 0usize;

        // Hop 0: the root symbol itself
        let root_ix = match snap.id_to_idx.get(&sid).copied() {
            Some(ix) => ix,
            None => {
                return (
                    format!("symbol '{qname}' not found in graph snapshot"),
                    true,
                );
            }
        };
        hops.push(json!({
            "depth": 0,
            "symbol": self.node_json(snap, root_ix, None),
            "callers": [],
            "callees": [],
        }));
        total_nodes += 1;

        // For each depth level, collect callers and callees separately
        let mut visited: std::collections::HashSet<SymbolId> = std::collections::HashSet::new();
        visited.insert(sid);
        let mut frontier: Vec<SymbolId> = vec![sid];

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
        if !self.has_manual_full_index() {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }

        // Lazy structural response
        let lazy_diag: Option<LazyDiagnostics> = lazy_outcome
            .as_ref()
            .map(LazyDiagnostics::from_structural);

        lr.with_precision_tier(tier)
            .with_lazy_warnings(lazy_warnings)
            .with_lazy_diag(lazy_diag)
            .build_with_args(resp, args, self)
    }

    pub(crate) fn handle_path(&mut self, args: &serde_json::Value) -> (String, bool) {
        let from_qname = get_str(args, "from");
        let to_qname = get_str(args, "to");
        if from_qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("from exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
                true,
            );
        }
        if to_qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("to exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
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

        let from_ids = match self.resolve_all_qname_symbols(from_qname) {
            Ok(ids) => ids,
            Err(e) => return (e, true),
        };
        let to_ids = match self.resolve_all_qname_symbols(to_qname) {
            Ok(ids) => ids,
            Err(e) => return (e, true),
        };

        // Update investigation with the first "from" symbol
        if let Some(&first_from) = from_ids.first() {
            self.update_investigation(InvestigationFocus::Symbol(first_from));
        }
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("path", args);
        let query_id = lr.query_id().to_string();

        // Transparent lazy structural: ensure both endpoint files have full
        // structural data before path finding.  A manifest-only index (MCP
        // default) may lack the intra-file call edges that BFS needs to
        // discover a path.
        let (roots, root_warnings) = self.include_roots_from_args(args);
        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        use std::collections::HashSet;
        let mut file_ids_set: HashSet<atlas_engine::FileId> = HashSet::new();
        for id in from_ids.iter().chain(to_ids.iter()) {
            if let Some(sym) = self.store.find_symbol_by_id(id).ok().flatten() {
                file_ids_set.insert(sym.file_id);
            }
        }
        let outcome = self.ensure_structural_for_files(
            file_ids_set,
            roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        let lazy_warnings = outcome.warnings;
        let lazy_diag = outcome
            .lazy_outcome
            .as_ref()
            .map(|lo| LazyDiagnostics::from_structural(lo));
        let tier = outcome.precision_tier;
        // Cache for no-path diagnostics below (used in user-facing messages).
        let is_manual_full = self.has_manual_full_index();

        let graph = self.context_builder().graph_snapshot();
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

            // Ambiguity metadata
            if from_ids.len() > 1 || to_ids.len() > 1 {
                let mut ambiguity = json!({});
                if from_ids.len() > 1 {
                    if let Some(ref wid) = winning_from {
                        ambiguity["matched_from"] = json!(symbol_label(&self.store, wid));
                    }
                    ambiguity["from_count"] = json!(from_ids.len());
                }
                if to_ids.len() > 1 {
                    if let Some(ref wid) = winning_to {
                        ambiguity["matched_to"] = json!(symbol_label(&self.store, wid));
                    }
                    ambiguity["to_count"] = json!(to_ids.len());
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

            lr.with_precision_tier(tier)
                .with_root_warnings(root_warnings)
                .with_lazy_warnings(lazy_warnings)
                .with_lazy_diag(lazy_diag)
                .with_partial_result(
                    tier != atlas_engine::structs::precision::PrecisionTier::Exact,
                )
                .build(resp, self)
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
            let resp = json!({
                "from": from_qname, "to": to_qname,
                "path_length": 0, "path": [], "breakpoints": [],
                "message": &message, "frontier": frontier_nodes,
            });

            lr.with_precision_tier(tier)
                .with_root_warnings(root_warnings)
                .with_lazy_warnings(lazy_warnings)
                .with_lazy_diag(lazy_diag)
                .with_partial_result(
                    tier != atlas_engine::structs::precision::PrecisionTier::Exact,
                )
                .build(resp, self)
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
        let qname = get_str(args, "symbol");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
                true,
            );
        }
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => {
                let mut err = format!("Lookup error: {e}");
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };
        let sym = match symbols.first() {
            Some(s) => s,
            None => {
                let mut err = format!("Symbol not found: {qname}");
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };

        self.update_investigation(InvestigationFocus::Symbol(sym.id));
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("explore", args);
        let query_id = lr.query_id().to_string();

        let source = if include_code {
            self.read_symbol_source(&sym.id)
        } else {
            None
        };

        // Lazy structural: ensure graph edges exist before querying
        let file_ids: Vec<atlas_engine::FileId> = std::iter::once(sym.file_id).collect();
        let outcome_files = self.ensure_structural_for_files(
            file_ids,
            vec![],
            investigation.as_ref(),
            Some(&query_id),
        );
        let outcome_name = self.ensure_structural_for_symbol_name(
            qname,
            vec![],
            investigation.as_ref(),
            Some(&query_id),
        );

        let graph = self.context_builder().graph_snapshot();
        let snap = graph.snapshot();

        // Immediate neighbors with edge kind info
        let incoming: Vec<_> = snap
            .incoming_neighbors_with_kinds(&sym.id)
            .iter()
            .map(|(node_ix, edge_kind)| {
                let n = snap.node(*node_ix);
                json!({
                    "name": n.name,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind.as_str(),
                    "edge_kind": edge_kind.as_str(),
                    "direction": "incoming",
                    "file": self.resolve_file_path(&n.file_id),
                    "line": n.start_line,
                })
            })
            .collect();

        let outgoing: Vec<_> = snap
            .outgoing_neighbors_with_kinds(&sym.id)
            .iter()
            .map(|(node_ix, edge_kind)| {
                let n = snap.node(*node_ix);
                json!({
                    "name": n.name,
                    "qualified_name": n.qualified_name,
                    "kind": n.kind.as_str(),
                    "edge_kind": edge_kind.as_str(),
                    "direction": "outgoing",
                    "file": self.resolve_file_path(&n.file_id),
                    "line": n.start_line,
                })
            })
            .collect();

        let mut sym_obj = json!({
            "name": sym.name,
            "qualified_name": sym.qualified_name,
            "kind": sym.kind.as_str(),
            "language": sym.language.as_str(),
            "file": self.resolve_file_path(&sym.file_id),
            "range": { "line": sym.range.start_line, "column": sym.range.start_column },
        });
        if let Some(ref src) = source {
            sym_obj["source"] = json!(src);
        }

        let mut resp = json!({
            "symbol": sym_obj,
            "incoming": incoming,
            "outgoing": outgoing,
        });
        if !self.has_manual_full_index() {
            resp["note"] = json!(
                "Structural data may be incomplete for manifest-only indexes. Run 'atlas index' or use 'symbol' (view='context') first for full results."
            );
        }

        // Lazy structural response — merge warnings from both outcomes
        let mut lazy_warnings: Vec<String> = outcome_files.warnings;
        lazy_warnings.extend(outcome_name.warnings);
        let tier = std::cmp::min(outcome_files.precision_tier, outcome_name.precision_tier);
        let lazy_diag: Option<LazyDiagnostics> = outcome_files
            .lazy_outcome
            .as_ref()
            .map(LazyDiagnostics::from_structural);

        lr.with_precision_tier(tier)
            .with_lazy_warnings(lazy_warnings)
            .with_lazy_diag(lazy_diag)
            .build_with_args(resp, args, self)
    }

    pub(crate) fn handle_impact(&mut self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {}", MAX_SYMBOL_NAME_LENGTH),
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
            None | Some(serde_json::Value::Null) => None, // use engine default
            Some(raw) => {
                let arr = match raw.as_array() {
                    Some(a) => a,
                    None => {
                        return ("edge_kinds must be an array of strings".to_string(), true);
                    }
                };
                if arr.is_empty() || (arr.len() == 1 && arr[0].as_str() == Some("*")) {
                    Some(vec![]) // wildcard → all edge kinds
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

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("impact", args);
        let query_id = lr.query_id().to_string();

        // Lazy structural: ensure graph edges exist before impact analysis
        let file_id = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.file_id);
        let file_ids: Vec<atlas_engine::FileId> = file_id.into_iter().collect();
        let outcome = self.ensure_structural_for_files(
            file_ids,
            vec![],
            investigation.as_ref(),
            Some(&query_id),
        );
        let lazy_diag: Option<LazyDiagnostics> = outcome
            .lazy_outcome
            .as_ref()
            .map(LazyDiagnostics::from_structural);

        let graph = self.context_builder().graph_snapshot();
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
            match self.store.list_domain_rules(None, None) {
                Ok(_rows) => {
                    let lang_str = self
                        .store
                        .find_symbol_by_id(&sid)
                        .ok()
                        .flatten()
                        .map(|s| s.language.as_str())
                        .unwrap_or("c");
                    Some(analysis::CppOwnershipRules::load_for(&self.store, lang_str))
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
                let cfg_nodes = match self.store.find_cfg_nodes_by_function(&node.symbol_id) {
                    Ok(nodes) => nodes,
                    Err(_) => continue,
                };
                if cfg_nodes.is_empty() {
                    continue;
                }

                // Run branch diff analysis
                let cfg_edges = self
                    .store
                    .find_cfg_edges_by_function(&node.symbol_id)
                    .unwrap_or_default();
                // ── Semantic branch diff with dataflow composition ──
                let lang = self
                    .store
                    .find_symbol_by_id(&node.symbol_id)
                    .ok()
                    .flatten()
                    .map(|s| s.language)
                    .unwrap_or(atlas_engine::Language::C);
                let contract = atlas_engine::analysis::ResourceOpConfig::default_for(lang);

                // Load DataFlow nodes and edges
                let data_nodes = self
                    .store
                    .find_data_nodes_by_function(&node.symbol_id)
                    .unwrap_or_default();
                let dataflow_edges = if data_nodes.is_empty() {
                    vec![]
                } else {
                    let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
                    self.store
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
        if semantic {
            resp["semantic_impact"] = json!({
                "invariants_affected": invariants,
                "lifecycle_paths_affected": lifecycle_paths,
                "domain_rules_applied": domain_rules.is_some(),
            });
        }
        if !self.has_manual_full_index() {
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

        let lazy_warnings = outcome.warnings;
        let tier = outcome.precision_tier;

        lr.with_precision_tier(tier)
            .with_root_warnings(Vec::new())
            .with_lazy_warnings(lazy_warnings)
            .with_lazy_diag(lazy_diag)
            .build(resp, self)
    }
}

#[cfg(test)]
mod tests {
    use super::EdgeKind;
    use super::parse_edge_kind;
    use crate::tools::ToolRouter;
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
        // With empty store, ensure returns early -> no warnings
        // Field may be absent (not printed) or present as empty array
        let warnings = resp.get("warnings");
        if let Some(w) = warnings {
            assert!(w.is_array(), "warnings should be an array");
            assert!(
                w.as_array().unwrap().is_empty(),
                "warnings should be empty for empty store"
            );
        }
        // precision_tier must be present
        assert!(
            resp.get("precision_tier").is_some(),
            "precision_tier field missing"
        );
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
        assert!(
            resp.get("precision_tier").is_some(),
            "precision_tier missing from callees response"
        );
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
        assert!(
            resp.get("precision_tier").is_some(),
            "precision_tier missing from callers response"
        );
    }
}
