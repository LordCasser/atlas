//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, capability.

use std::path::Path;
use std::sync::Arc;

use atlas_context::ContextBuilder;
use atlas_db::Store;
use atlas_graph::GraphEngine;
use atlas_search::SearchEngine;
use atlas_types::SymbolId;
use atlas_types::ids::FileId;

use super::protocol::{CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema};

use serde_json::{Value, json};

// -------------------------------------------------------------------
// Sub-modules — one per capability category
// -------------------------------------------------------------------

pub(crate) mod capability;
pub(crate) mod context;
pub(crate) mod graph;
pub(crate) mod search;
pub(crate) mod status;
pub(crate) mod trace;

// -------------------------------------------------------------------
// ToolRouter
// -------------------------------------------------------------------

/// Dispatches tools/list and tools/call.
pub struct ToolRouter {
    pub(crate) store: Arc<Store>,
    pub(crate) search: SearchEngine,
    pub(crate) context: ContextBuilder,
    /// Lazily-rebuilt GraphEngine per request (from fresh snapshot).
    /// Returns an error string on failure so the MCP server doesn't panic.
    pub(crate) graph_fn: Box<dyn Fn() -> Result<GraphEngine, String> + Send + Sync>,
    /// Project root directory for snippet extraction.
    pub(crate) project_root: std::path::PathBuf,
    tools: Vec<Tool>,
}

impl ToolRouter {
    pub fn new(
        store: Arc<Store>,
        search: SearchEngine,
        context: ContextBuilder,
        graph_fn: impl Fn() -> Result<GraphEngine, String> + Send + Sync + 'static,
        project_root: std::path::PathBuf,
    ) -> Self {
        let tools = make_all_tools();
        Self {
            store,
            search,
            context,
            graph_fn: Box::new(graph_fn),
            project_root,
            tools,
        }
    }

    /// Access the underlying store (for testing).
    pub fn store(&self) -> Arc<Store> {
        self.store.clone()
    }

    /// Handle tools/list — return all registered tool definitions.
    pub fn list_tools(&self) -> ListToolsResult {
        ListToolsResult {
            tools: self.tools.clone(),
        }
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
            "atlas_trace_point" => self.handle_trace_point(arguments),
            "atlas_trace_variable" => self.handle_trace_variable(arguments),
            "atlas_trace_caller_path" => self.handle_trace_caller_path(arguments),
            "atlas_language_capabilities" => self.handle_language_capabilities(),
            _ => (format!("Unknown tool: {}", name), true),
        };

        // Wrap long results with truncation warning
        let text = truncate(&result, 25000);
        let mut content = vec![ContentBlock::text(text)];
        if result.len() > 25000 {
            content.push(ContentBlock::text(format!(
                "(truncated — {} chars total, showing first 25000)",
                result.len()
            )));
        }

        CallToolResult {
            content,
            is_error: Some(is_error),
        }
    }

    // -------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------

    /// Rebuild the graph snapshot, returning an error message on failure.
    /// This avoids panicking when the graph snapshot can't be reloaded.
    pub(crate) fn get_graph(&self) -> Result<GraphEngine, String> {
        (self.graph_fn)()
    }

    /// Generate a structured JSON error for graph rebuild failures.
    pub(crate) fn graph_error_result(err: &str) -> (String, bool) {
        (
            json!({ "ok": false, "error": "graph_reload_failed", "message": err }).to_string(),
            true,
        )
    }

    /// Resolve a qualified name to a SymbolId, returning error string on failure.
    pub(crate) fn resolve_qname(&self, qname: &str) -> Result<SymbolId, String> {
        let symbols = self
            .store
            .find_symbols_by_qname(qname)
            .map_err(|e| format!("Lookup error: {}", e))?;
        symbols
            .first()
            .map(|s| s.id)
            .ok_or_else(|| format!("Symbol not found: {}", qname))
    }

    /// Render a node from the graph snapshot to JSON.
    pub(crate) fn node_json(snap: &atlas_graph::GraphSnapshot, ix: atlas_graph::NodeIx) -> Value {
        let n = snap.node(ix);
        json!({
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
        })
    }
}

// -------------------------------------------------------------------
// Tool registration
// -------------------------------------------------------------------

pub fn make_all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "atlas_status".into(),
            description: "Show project overview: file/symbol/edge counts, DB stats, per-language capability profiles.".into(),
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
        Tool {
            name: "atlas_trace_point".into(),
            description: "Resolve a source position (file_id or file_path + line + column) to its full context: reference, symbol, data node, scope, bindings, and incident dataflow edges.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex (from atlas_files)" },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts')" },
                    "line": { "type": "integer", "description": "1-based line number" },
                    "column": { "type": "integer", "description": "1-based column number" },
                })),
                required: None,
            },
        },
        Tool {
            name: "atlas_trace_variable".into(),
            description: "Trace where a variable's value comes from. Walks backward through dataflow edges from a source position to find origins (parameters, literals, globals). Returns the full trace path with steps.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex (from atlas_files)" },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts')" },
                    "line": { "type": "integer", "description": "1-based line number" },
                    "column": { "type": "integer", "description": "1-based column number" },
                    "max_depth": { "type": "integer", "description": "Maximum backward traversal depth (default 30)" },
                })),
                required: None,
            },
        },
        Tool {
            name: "atlas_trace_caller_path".into(),
            description: "Trace how a function gets invoked. Walks backward through call edges (Calls/Instantiates/Implements) from a target symbol to its farthest caller. Returns the full caller chain.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Symbol ID in hex (from atlas_search or atlas_symbol)" },
                    "symbol_name": { "type": "string", "description": "Symbol name for lookup (e.g. 'inner'). Alternative to 'symbol' hex ID." },
                    "max_depth": { "type": "integer", "description": "Maximum backward call depth (default 20)" },
                })),
                required: None,
            },
        },
        Tool {
            name: "atlas_language_capabilities".into(),
            description: "Show per-language analysis capability profiles: supported features, limitations, confidence floor.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
    ]
}

// -------------------------------------------------------------------
// Shared arg-parsing helpers
// -------------------------------------------------------------------

/// Resolve a file_id from either a hex string or a file_path string.
/// Returns `Ok(None)` when neither is provided; `Err` on lookup failure.
pub(crate) fn resolve_file_id(
    store: &Store,
    root: &Path,
    file_hex: Option<&str>,
    file_path: Option<&str>,
) -> anyhow::Result<Option<FileId>> {
    // 1. Try hex file_id
    if let Some(hex) = file_hex.and_then(|h| {
        let h = h.trim();
        if h.len() >= 8 {
            h.parse::<FileId>().ok()
        } else {
            None
        }
    }) {
        return Ok(Some(hex));
    }
    // 2. Try file_path match (delegates to indexed Store lookup).
    if let Some(path) = file_path {
        let clean = path.trim_start_matches("./").trim_start_matches('/');
        return store.resolve_file_id(root, clean);
    }
    Ok(None)
}

pub(crate) fn get_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

pub(crate) fn get_str_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub(crate) fn get_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len { s } else { &s[..max_len] }
}
