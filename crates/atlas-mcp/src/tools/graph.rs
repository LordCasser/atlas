//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use atlas_engine::{EdgeKind, SymbolId, TraversalConfig, TraversalDirection};

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
            .map(|ix| Self::node_json(snap, *ix))
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

        let nodes: Vec<_> = shown.map(|ix| Self::node_json(snap, *ix)).collect();

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

        let nodes: Vec<_> = shown.map(|ix| Self::node_json(snap, *ix)).collect();

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
        hops.push(json!({
            "depth": 0,
            "symbol": Self::node_json(snap, snap.id_to_idx.get(&sid).copied().unwrap_or(0)),
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
                        "file_id": snap.node(neighbor_ix).file_id.short_hex(),
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
                        "file_id": snap.node(neighbor_ix).file_id.short_hex(),
                    }));
                }
            }

            hop_callers.truncate(limit.saturating_sub(total_nodes));
            hop_callees.truncate(limit.saturating_sub(total_nodes.saturating_add(hop_callers.len())));
            total_nodes = total_nodes.saturating_add(hop_callers.len()).saturating_add(hop_callees.len());

            hops.push(json!({
                "depth": d,
                "callers": hop_callers,
                "callees": hop_callees,
                "caller_count": hop_callers.len(),
                "callee_count": hop_callees.len(),
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

    pub(crate) fn handle_path(&self, args: &serde_json::Value) -> (String, bool) {
        let from_qname = get_str(args, "from");
        let to_qname = get_str(args, "to");
        let max_depth = get_u64(args, "max_depth").unwrap_or(5) as usize;

        let from_id = match self.resolve_qname(from_qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };
        let to_id = match self.resolve_qname(to_qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = self.context_builder().graph_snapshot();
        match graph.shortest_path(&from_id, &to_id, max_depth.min(10)) {
            Some(path) => {
                let snap = graph.snapshot();
                let nodes: Vec<_> = path
                    .node_indices
                    .iter()
                    .map(|ix| Self::node_json(snap, *ix))
                    .collect();
                (
                    serde_json::to_string_pretty(&json!({
                        "from": from_qname,
                        "to": to_qname,
                        "path_length": nodes.len(),
                        "path": nodes,
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            None => (
                serde_json::to_string_pretty(&json!({
                    "from": from_qname,
                    "to": to_qname,
                    "path_length": 0,
                    "path": [],
                    "message": "No path found within depth limit",
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
        }
    }

    pub(crate) fn handle_explore(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");

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
                })
            })
            .collect();

        (
            serde_json::to_string_pretty(&json!({
                "symbol": {
                    "name": sym.name,
                    "qualified_name": sym.qualified_name,
                    "kind": sym.kind.as_str(),
                    "language": sym.language.as_str(),
                    "file": self.resolve_file_path(&sym.file_id),
                    "file_id": sym.file_id.to_hex(),
                    "range": { "line": sym.range.start_line, "column": sym.range.start_column },
                },
                "neighbors": {
                    "incoming_count": incoming.len(),
                    "outgoing_count": outgoing.len(),
                    "incoming": incoming,
                    "outgoing": outgoing,
                },
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

        let nodes: Vec<_> = sub
            .node_indices
            .iter()
            .take(30)
            .map(|ix| Self::node_json(snap, *ix))
            .collect();

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "max_depth": depth,
                "impacted_nodes": nodes.len(),
                "nodes": nodes,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
