//! Graph traversal tools: neighbors, callers, callees, callgraph, path,
//! explore, and impact analysis.

use atlas_graph::{TraversalConfig, TraversalDirection};

use super::{ToolRouter, get_str, get_str_opt, get_u64};

use serde_json::json;

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

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
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

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
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

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
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

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
        let sub = graph.callgraph(&sid, depth.min(5));
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
                "max_depth": depth,
                "nodes_found": sub.node_indices.len(),
                "nodes": nodes,
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

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
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
            Err(e) => return (format!("Lookup error: {}", e), true),
        };
        let sym = match symbols.first() {
            Some(s) => s,
            None => return (format!("Symbol not found: {}", qname), true),
        };

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
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
                    "file": sym.file_id.to_hex(),
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

        let graph = match self.get_graph() {
            Ok(g) => g,
            Err(e) => return Self::graph_error_result(&e),
        };
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
