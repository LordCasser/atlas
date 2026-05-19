//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.

use std::sync::Arc;

use crate::db::Store;
use crate::graph::{GraphEngine, TraversalConfig, TraversalDirection};
use crate::search::SearchEngine;
use crate::context::ContextBuilder;
use crate::types::{SymbolId, SymbolKind};

use super::protocol::{
    CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema,
};

use serde_json::{json, Value};

// -------------------------------------------------------------------
// ToolRouter
// -------------------------------------------------------------------

/// Dispatches tools/list and tools/call.
pub struct ToolRouter {
    store: Arc<Store>,
    search: SearchEngine,
    context: ContextBuilder,
    /// Lazily-rebuilt GraphEngine per request (from fresh snapshot).
    graph_fn: Box<dyn Fn() -> GraphEngine + Send + Sync>,
    tools: Vec<Tool>,
}

impl ToolRouter {
    pub fn new(
        store: Arc<Store>,
        search: SearchEngine,
        context: ContextBuilder,
        graph_fn: impl Fn() -> GraphEngine + Send + Sync + 'static,
    ) -> Self {
        let tools = make_all_tools();
        Self { store, search, context, graph_fn: Box::new(graph_fn), tools }
    }

    /// Handle tools/list — return all registered tool definitions.
    pub fn list_tools(&self) -> ListToolsResult {
        ListToolsResult { tools: self.tools.clone() }
    }

    /// Handle tools/call — dispatch by tool name.
    pub fn call_tool(&self, name: &str, arguments: &Value) -> CallToolResult {
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let (result, is_error) = match name {
            "atlas_status" => self.handle_status(),
            "atlas_files" => self.handle_files(),
            "atlas_search" => self.handle_search(arguments),
            "atlas_symbol" => self.handle_symbol(arguments),
            "atlas_neighbors" => self.handle_neighbors(arguments),
            "atlas_callers" => self.handle_callers(arguments),
            "atlas_callees" => self.handle_callees(arguments),
            "atlas_callgraph" => self.handle_callgraph(arguments),
            "atlas_path" => self.handle_path(arguments),
            "atlas_explore" => self.handle_explore(arguments),
            "atlas_impact" => self.handle_impact(arguments),
            "atlas_context" => self.handle_context(arguments),
            _ => (format!("Unknown tool: {}", name), true),
        };

        // Wrap long results with truncation warning
        let text = truncate(&result, 8000);
        let mut content = vec![ContentBlock::text(text)];
        if result.len() > 8000 {
            content.push(ContentBlock::text(format!(
                "(truncated — {} chars total, showing first 8000)",
                result.len()
            )));
        }

        CallToolResult { content, is_error: Some(is_error) }
    }

    // -------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------

    /// Resolve a qualified name to a SymbolId, returning error string on failure.
    fn resolve_qname(&self, qname: &str) -> Result<SymbolId, String> {
        let symbols = self.store.find_symbols_by_qname(qname)
            .map_err(|e| format!("Lookup error: {}", e))?;
        symbols.first()
            .map(|s| s.id)
            .ok_or_else(|| format!("Symbol not found: {}", qname))
    }

    /// Render a node from the graph snapshot to JSON.
    fn node_json(snap: &crate::graph::GraphSnapshot, ix: crate::graph::NodeIx) -> Value {
        let n = snap.node(ix);
        json!({
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
        })
    }

    // -------------------------------------------------------------------
    // Tool handlers — each returns (result_text, is_error)
    // -------------------------------------------------------------------

    fn handle_status(&self) -> (String, bool) {
        let stats = match self.store.get_stats() {
            Ok(s) => s,
            Err(e) => return (format!("Error getting stats: {}", e), true),
        };
        (serde_json::to_string_pretty(&json!({
            "summary": {
                "files": stats.total_files,
                "symbols": stats.total_symbols,
                "references": stats.total_references,
                "edges": stats.total_edges,
                "unresolved_references": stats.unresolved_references,
            },
            "database": {
                "sqlite_version": stats.sqlite_version,
            }
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_files(&self) -> (String, bool) {
        match self.store.list_files() {
            Ok(files) => {
                (serde_json::to_string_pretty(&json!({
                    "count": files.len(),
                    "files": files.iter().map(|f| json!({
                        "path": f.path,
                        "language": f.language.as_str(),
                        "status": f.status.as_str(),
                    })).collect::<Vec<_>>(),
                })).unwrap_or_else(|e| e.to_string()), false)
            }
            Err(e) => (format!("Error listing files: {}", e), true),
        }
    }

    fn handle_search(&self, args: &Value) -> (String, bool) {
        let query = get_str(args, "query");
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;
        let kind = get_str_opt(args, "kind");

        let results = if let Some(k_str) = kind {
            match SymbolKind::from_str(k_str) {
                Some(k) => self.search.search_by_kind(query, k, limit),
                None => return (format!("Unknown symbol kind: {}", k_str), true),
            }
        } else {
            self.search.search(query, limit)
        };

        match results {
            Ok(entries) => {
                (serde_json::to_string_pretty(&json!({
                    "query": query,
                    "count": entries.len(),
                    "results": entries.iter().map(|e| json!({
                        "name": e.symbol.name,
                        "qualified_name": e.symbol.qualified_name,
                        "kind": e.symbol.kind.as_str(),
                        "language": e.symbol.language.as_str(),
                        "score": e.score.total,
                        "file": e.symbol.file_id.to_hex(),
                    })).collect::<Vec<_>>(),
                })).unwrap_or_else(|e| e.to_string()), false)
            }
            Err(e) => (format!("Search error: {}", e), true),
        }
    }

    fn handle_symbol(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "qualified_name");
        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => return (format!("Lookup error: {}", e), true),
        };
        let sym = match symbols.first() {
            Some(s) => s,
            None => return (format!("Symbol not found: {}", qname), true),
        };

        let graph = (self.graph_fn)();
        let callers_count = graph.callers(&sym.id).callers.len();
        let callees_count = graph.callees(&sym.id).callees.len();

        (serde_json::to_string_pretty(&json!({
            "name": sym.name,
            "qualified_name": sym.qualified_name,
            "kind": sym.kind.as_str(),
            "language": sym.language.as_str(),
            "visibility": sym.visibility.as_ref().map(|v| v.as_str()),
            "signature": sym.signature,
            "file": sym.file_id.to_hex(),
            "range": {
                "line": sym.range.start_line,
                "column": sym.range.start_column,
            },
            "callers": callers_count,
            "callees": callees_count,
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_neighbors(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let direction = get_str_opt(args, "direction").unwrap_or("both");
        let depth = get_u64(args, "depth").unwrap_or(1) as usize;
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = (self.graph_fn)();
        let dir = match direction {
            "outgoing" => TraversalDirection::Outgoing,
            "incoming" => TraversalDirection::Incoming,
            _ => TraversalDirection::Both,
        };

        let sub = graph.neighbors(&sid, TraversalConfig {
            direction: dir,
            max_depth: depth.min(3),
            limit: limit.min(100),
            edge_kind_filter: None,
        });

        let snap = graph.snapshot();
        let nodes: Vec<_> = sub.node_indices.iter().take(limit)
            .map(|ix| Self::node_json(snap, *ix))
            .collect();

        (serde_json::to_string_pretty(&json!({
            "symbol": qname,
            "direction": direction,
            "depth": depth,
            "nodes": nodes,
            "total_found": sub.node_indices.len(),
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_callers(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = (self.graph_fn)();
        let cg = graph.callers(&sid);
        let snap = graph.snapshot();
        let shown = cg.callers.iter().take(limit);

        let nodes: Vec<_> = shown
            .map(|ix| Self::node_json(snap, *ix))
            .collect();

        (serde_json::to_string_pretty(&json!({
            "symbol": qname,
            "total_callers": cg.callers.len(),
            "callers": nodes,
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_callees(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let limit = get_u64(args, "limit").unwrap_or(20) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = (self.graph_fn)();
        let cg = graph.callees(&sid);
        let snap = graph.snapshot();
        let shown = cg.callees.iter().take(limit);

        let nodes: Vec<_> = shown
            .map(|ix| Self::node_json(snap, *ix))
            .collect();

        (serde_json::to_string_pretty(&json!({
            "symbol": qname,
            "total_callees": cg.callees.len(),
            "callees": nodes,
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_callgraph(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;
        let limit = get_u64(args, "limit").unwrap_or(100) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = (self.graph_fn)();
        let sub = graph.callgraph(&sid, depth.min(5));
        let snap = graph.snapshot();

        let nodes: Vec<_> = sub.node_indices.iter().take(limit)
            .map(|ix| Self::node_json(snap, *ix))
            .collect();

        (serde_json::to_string_pretty(&json!({
            "symbol": qname,
            "max_depth": depth,
            "nodes_found": sub.node_indices.len(),
            "nodes": nodes,
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_path(&self, args: &Value) -> (String, bool) {
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

        let graph = (self.graph_fn)();
        match graph.shortest_path(&from_id, &to_id, max_depth.min(10)) {
            Some(path) => {
                let snap = graph.snapshot();
                let nodes: Vec<_> = path.node_indices.iter()
                    .map(|ix| Self::node_json(snap, *ix))
                    .collect();
                (serde_json::to_string_pretty(&json!({
                    "from": from_qname,
                    "to": to_qname,
                    "path_length": nodes.len(),
                    "path": nodes,
                })).unwrap_or_else(|e| e.to_string()), false)
            }
            None => {
                (serde_json::to_string_pretty(&json!({
                    "from": from_qname,
                    "to": to_qname,
                    "path_length": 0,
                    "path": [],
                    "message": "No path found within depth limit",
                })).unwrap_or_else(|e| e.to_string()), false)
            }
        }
    }

    fn handle_explore(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");

        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => return (format!("Lookup error: {}", e), true),
        };
        let sym = match symbols.first() {
            Some(s) => s,
            None => return (format!("Symbol not found: {}", qname), true),
        };

        let graph = (self.graph_fn)();
        let snap = graph.snapshot();

        // Immediate neighbors with edge kind info
        let incoming: Vec<_> = snap.incoming_neighbors_with_kinds(&sym.id)
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

        let outgoing: Vec<_> = snap.outgoing_neighbors_with_kinds(&sym.id)
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

        (serde_json::to_string_pretty(&json!({
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
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_impact(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let depth = get_u64(args, "depth").unwrap_or(3) as usize;

        let sid = match self.resolve_qname(qname) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        let graph = (self.graph_fn)();
        let sub = graph.impact(&sid, depth.min(5));
        let snap = graph.snapshot();

        let nodes: Vec<_> = sub.node_indices.iter().take(30)
            .map(|ix| Self::node_json(snap, *ix))
            .collect();

        (serde_json::to_string_pretty(&json!({
            "symbol": qname,
            "max_depth": depth,
            "impacted_nodes": nodes.len(),
            "nodes": nodes,
        })).unwrap_or_else(|e| e.to_string()), false)
    }

    fn handle_context(&self, args: &Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => return (format!("Lookup error: {}", e), true),
        };
        let sid = match symbols.first().map(|s| s.id) {
            Some(id) => id,
            None => return (format!("Symbol not found: {}", qname), true),
        };
        match self.context.build_context_for_symbol(&sid) {
            Ok(view) => {
                // Wrap markdown in JSON so it's not misdetected as error
                let md = view.to_markdown();
                (serde_json::to_string_pretty(&json!({
                    "markdown": md,
                })).unwrap_or_else(|e| e.to_string()), false)
            }
            Err(e) => (format!("Context build error: {}", e), true),
        }
    }
}

// -------------------------------------------------------------------
// Tool registration
// -------------------------------------------------------------------

fn make_all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "atlas_status".into(),
            description: "Show project overview: file/symbol/edge counts, DB stats.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "atlas_files".into(),
            description: "List all indexed files with language and parse status.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "atlas_search".into(),
            description: "Search symbols by name (FTS5 + fuzzy). Supports kind filter.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query": { "type": "string", "description": "Search query text" },
                    "kind": { "type": "string", "description": "Optional SymbolKind filter (function, class, ...)" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                })),
                required: Some(vec!["query".into()]),
            },
        },
        Tool {
            name: "atlas_symbol".into(),
            description: "Get detailed info for a symbol by qualified name with caller/callee counts.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "qualified_name": { "type": "string", "description": "Fully qualified symbol name" },
                })),
                required: Some(vec!["qualified_name".into()]),
            },
        },
        Tool {
            name: "atlas_neighbors".into(),
            description: "Get graph neighbors of a symbol (all edge kinds, configurable direction/depth).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "direction": { "type": "string", "description": "outgoing / incoming / both (default both)" },
                    "depth": { "type": "integer", "description": "Traversal depth (default 1, max 3)" },
                    "limit": { "type": "integer", "description": "Max nodes returned (default 50)" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "atlas_callers".into(),
            description: "List symbols that call a given symbol (incoming Calls/Instantiates/Implements edges).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "atlas_callees".into(),
            description: "List symbols called by a given symbol (outgoing Calls/Instantiates/Implements edges).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "atlas_callgraph".into(),
            description: "Build call graph around a symbol: all callers and callees up to configurable depth (BFS).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "depth": { "type": "integer", "description": "Max traversal depth (default 3, max 5)" },
                    "limit": { "type": "integer", "description": "Max nodes returned (default 100)" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "atlas_path".into(),
            description: "Find the shortest path between two symbols through the graph (BFS).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "from": { "type": "string", "description": "Source symbol qualified name" },
                    "to": { "type": "string", "description": "Target symbol qualified name" },
                    "max_depth": { "type": "integer", "description": "Max search depth (default 5, max 10)" },
                })),
                required: Some(vec!["from".into(), "to".into()]),
            },
        },
        Tool {
            name: "atlas_explore".into(),
            description: "Explore a symbol: detail info + all immediate neighbors grouped by edge kind.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "atlas_impact".into(),
            description: "Compute impact analysis: all symbols reachable from a given symbol (BFS outward).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "depth": { "type": "integer", "description": "Max traversal depth (default 3, max 5)" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "atlas_context".into(),
            description: "Build rich context for a symbol: callers, callees, imports, file peers (markdown).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// -------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------

fn get_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn get_str_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn get_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len { s } else { &s[..max_len] }
}
