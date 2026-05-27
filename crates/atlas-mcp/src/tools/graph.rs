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

        let edge_kind_filter = match Self::resolve_path_edge_kinds(args) {
            Ok(f) => f,
            Err(e) => return (e, true),
        };

        let from_id = match self.resolve_qname(from_qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };
        let to_id = match self.resolve_qname(to_qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = self.context_builder().graph_snapshot();
        match graph.shortest_path(&from_id, &to_id, max_depth.min(10), edge_kind_filter.as_deref()) {
            Some(path) => {
                let snap = graph.snapshot();
                let mut hops: Vec<serde_json::Value> = Vec::with_capacity(
                    path.node_indices.len() + path.edge_indices.len(),
                );
                for i in 0..path.node_indices.len() {
                    hops.push(Self::node_json(snap, path.node_indices[i]));
                    if i < path.edge_indices.len() {
                        let edge = snap.edge(path.edge_indices[i]);
                        hops.push(json!({
                            "edge_kind": edge.kind.as_str(),
                        }));
                    }
                }
                (
                    serde_json::to_string_pretty(&json!({
                        "from": from_qname,
                        "to": to_qname,
                        "path_length": path.node_indices.len(),
                        "path": hops,
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
                    "range": { "line": sym.range.start_line, "column": sym.range.start_column },
                },
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
