//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use atlas_engine::{EdgeKind, Store, SymbolId, TraversalConfig, TraversalDirection};

use super::{ToolRouter, get_str, get_str_opt, get_u64};

use serde_json::json;

/// Check if an edge kind represents a call relationship.
fn is_call_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::Instantiates
            | EdgeKind::Implements
            | EdgeKind::RegistersCallback
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
            "Unknown edge kind: '{}'. Valid kinds: calls, instantiates, implements, registers_callback, references, contains, imports, includes, exports, extends, typeof, returns, overrides, decorates, defines, argument, parameter, assigns, reads, writes, field_read, field_write",
            s
        )),
    }
}

impl ToolRouter {
    pub(crate) fn handle_neighbors(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let direction = get_str_opt(args, "direction").unwrap_or("both");
        let depth = get_u64(args, "depth").unwrap_or(1) as usize;
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = self.context_builder().graph_snapshot();
        let dir = match direction {
            "outgoing" => TraversalDirection::Outgoing,
            "incoming" => TraversalDirection::Incoming,
            _ => TraversalDirection::Both,
        };

        let sub = graph.neighbors(
            &sid,
            TraversalConfig {
                direction: dir,
                max_depth: depth.min(3),
                limit: limit.min(100),
                edge_kind_filter: None,
            },
        );

        let snap = graph.snapshot();
        let nodes: Vec<_> = sub
            .node_indices
            .iter()
            .take(limit)
            .map(|ix| self.node_json(snap, *ix))
            .collect();

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "direction": direction,
                "depth": depth,
                "nodes": nodes,
                "total_found": sub.node_indices.len(),
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    pub(crate) fn handle_callers(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = self.context_builder().graph_snapshot();
        let cg = graph.callers(&sid);
        let snap = graph.snapshot();
        let shown = cg.callers.iter().take(limit);

        let nodes: Vec<_> = shown.map(|ix| self.node_json(snap, *ix)).collect();

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "total_callers": cg.callers.len(),
                "callers": nodes,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    pub(crate) fn handle_callees(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = self.context_builder().graph_snapshot();
        let cg = graph.callees(&sid);
        let snap = graph.snapshot();
        let shown = cg.callees.iter().take(limit);

        let nodes: Vec<_> = shown.map(|ix| self.node_json(snap, *ix)).collect();

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "total_callees": cg.callees.len(),
                "callees": nodes,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    pub(crate) fn handle_callgraph(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;
        let limit = get_u64(args, "limit").unwrap_or(100) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
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
                    format!("symbol '{}' not found in graph snapshot", qname),
                    true,
                );
            }
        };
        hops.push(json!({
            "depth": 0,
            "symbol": self.node_json(snap, root_ix),
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

            for fid in &frontier {
                // Incoming edges → callers
                for (neighbor_ix, edge_kind) in snap.incoming_neighbors_with_kinds(fid) {
                    let neighbor_id = snap.node(neighbor_ix).symbol_id;
                    if visited.contains(&neighbor_id) {
                        continue;
                    }
                    // Only include call-related edges
                    if !is_call_edge(&edge_kind) {
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
                // Outgoing edges → callees
                for (neighbor_ix, edge_kind) in snap.outgoing_neighbors_with_kinds(fid) {
                    let neighbor_id = snap.node(neighbor_ix).symbol_id;
                    if visited.contains(&neighbor_id) {
                        continue;
                    }
                    if !is_call_edge(&edge_kind) {
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

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "max_depth": depth,
                "total_nodes_visited": total_nodes,
                "hops": hops,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    pub(crate) fn handle_path(&mut self, args: &serde_json::Value) -> (String, bool) {
        let from_qname = get_str(args, "from");
        let to_qname = get_str(args, "to");
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
        let lazy_warnings = self.ensure_structural_for_files(file_ids_set, roots);
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
                let mut node_json = tool.node_json(snap, path.node_indices[i]);
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
            let hops = build_hops(self, &snap, &primary.path, include_code);
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
                        let alt_hops = build_hops(self, &snap, &r.path, false);
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
                                    "atlas_annotate_fp_dispatch(field_qname='{}...', target_qname='{}')",
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
                    "The primary path is likely blocked by unresolved function pointers. Use 'atlas_annotate_fp_dispatch' to declare known dispatches (e.g., curl handler tables, vtable assignments), then re-run the path query after annotation materialization."
                );
            }

            resp["path_quality"] = insight;

            // Surface include_roots and lazy-structural warnings to the caller.
            let mut all_warnings: Vec<String> = root_warnings;
            all_warnings.extend(lazy_warnings);
            if !all_warnings.is_empty() {
                resp["warnings"] = json!(all_warnings);
            }

            (
                serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                false,
            )
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
            // Surface include_roots and lazy-structural warnings.
            let mut all_warnings: Vec<String> = root_warnings;
            all_warnings.extend(lazy_warnings);
            (
                serde_json::to_string_pretty(&json!({
                    "from": from_qname, "to": to_qname,
                    "path_length": 0, "path": [], "breakpoints": [],
                    "message": &message, "frontier": frontier_nodes,
                    "warnings": all_warnings,
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            )
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
        let qname = get_str(args, "symbol");
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => {
                let mut err = format!("Lookup error: {}", e);
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };
        let sym = match symbols.first() {
            Some(s) => s,
            None => {
                let mut err = format!("Symbol not found: {}", qname);
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };

        let source = if include_code {
            self.read_symbol_source(&sym.id)
        } else {
            None
        };

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

        (
            serde_json::to_string_pretty(&json!({
                "symbol": sym_obj,
                "incoming": incoming,
                "outgoing": outgoing,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    pub(crate) fn handle_impact(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = self.context_builder().graph_snapshot();
        let sub = graph.impact(&sid, depth.min(5));
        let snap = graph.snapshot();

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

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "max_depth": depth,
                "impacted_nodes": total_shown,
                "file_groups": grouped,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
