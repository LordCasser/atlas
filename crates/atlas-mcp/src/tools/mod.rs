//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, capability.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use atlas_engine::ContextBuilder;
use atlas_engine::FileId;
use atlas_engine::LazyDataflowService;
use atlas_engine::SearchEngine;
use atlas_engine::Store;
use atlas_engine::SymbolId;

use super::protocol::{CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema};

use serde_json::{Value, json};

/// Progress report tuple: (progress, total, message)
pub(crate) type ProgressReport = (f64, Option<f64>, Option<String>);
/// Channel sender for progress updates during long-running operations.
pub(crate) type ProgressSender = tokio::sync::mpsc::UnboundedSender<ProgressReport>;

/// Prepared project state produced by `open_project(background=true)`.
///
/// The background worker cannot mutate the live router safely, so it stores the
/// prepared store/root here. `task_status` and `wait_for_task` activate it after
/// the task reaches `completed`.
pub(crate) struct PendingProjectActivation {
    pub(crate) project_root: std::path::PathBuf,
    pub(crate) store: Arc<Store>,
}

// -------------------------------------------------------------------
// Sub-modules — one per capability category
// -------------------------------------------------------------------

pub(crate) mod capability;
pub(crate) mod context;
pub(crate) mod dependencies;
pub(crate) mod dependents;
pub(crate) mod graph;
pub(crate) mod index;
pub(crate) mod open_project;
pub(crate) mod search;
pub(crate) mod status;
pub(crate) mod trace;
pub(crate) mod usages;
pub(crate) mod wait_for;

// -------------------------------------------------------------------
// ToolRouter
// -------------------------------------------------------------------

/// Dispatches tools/list and tools/call.
pub struct ToolRouter {
    pub(crate) store: Arc<Store>,
    pub(crate) lazy_service: LazyDataflowService,
    /// Graph engines built lazily on first request (after MCP handshake).
    pub(crate) search: Option<SearchEngine>,
    pub(crate) context: Option<ContextBuilder>,
    /// Project root directory for snippet extraction.
    pub(crate) project_root: std::path::PathBuf,
    tools: Vec<Tool>,
    /// Database/index signature at last graph build (used to detect external index/sync).
    last_graph_signature: String,
    /// True once the graph has been built at least once.
    graph_initialized: bool,
    /// Cached signature to avoid per-request COUNT queries.
    cached_signature: String,
    /// When the cached signature was last checked (avoids re-query within cooldown).
    last_signature_check: std::time::Instant,
    /// Optional progress sender for long-running operations (set per-call in lib.rs).
    pub(crate) progress_sender: Option<ProgressSender>,
    /// Background task manager for `background: true` mode.
    pub(crate) task_manager: Arc<crate::task_manager::TaskManager>,
    /// Project activations prepared by background `open_project` tasks.
    pub(crate) pending_project_activations: Arc<Mutex<HashMap<String, PendingProjectActivation>>>,
}

impl ToolRouter {
    /// Create a router with pre-built graph-backed engines.
    ///
    /// Integration tests use this constructor to exercise tool routing against a
    /// known graph snapshot. The stdio MCP server uses [`ToolRouter::new_empty`]
    /// so startup and `initialize` do not block on graph construction.
    pub fn new(
        store: Arc<Store>,
        search: SearchEngine,
        context: ContextBuilder,
        project_root: std::path::PathBuf,
    ) -> Self {
        let last_graph_signature = store.index_signature().unwrap_or_default();
        let lazy_service = LazyDataflowService::new(store.clone(), Some(project_root.clone()));
        Self {
            store: store.clone(),
            lazy_service,
            search: Some(search),
            context: Some(context),
            project_root,
            tools: make_all_tools(),
            last_graph_signature: last_graph_signature.clone(),
            graph_initialized: true,
            cached_signature: last_graph_signature,
            last_signature_check: std::time::Instant::now(),
            progress_sender: None,
            task_manager: Arc::new(crate::task_manager::TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a router without building the graph (fast startup).
    /// Graph is built lazily on the first request via `ensure_graph_initialized`.
    pub fn new_empty(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        let tools = make_all_tools();
        let lazy_service = LazyDataflowService::new(store.clone(), Some(project_root.clone()));
        Self {
            store: store.clone(),
            lazy_service,
            search: None,
            context: None,
            project_root,
            tools,
            last_graph_signature: String::new(),
            graph_initialized: false,
            cached_signature: String::new(),
            last_signature_check: std::time::Instant::now(),
            progress_sender: None,
            task_manager: Arc::new(crate::task_manager::TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return the backing store.
    pub fn store(&self) -> Arc<Store> {
        Arc::clone(&self.store)
    }

    /// Return whether a tool needs the in-memory graph/search/context snapshot.
    ///
    /// Store-backed tools intentionally do not force graph construction. This
    /// keeps MCP `initialize`, `tools/list`, status, files, trace, usages,
    /// dependencies, dependents and capabilities responsive on large projects.
    pub fn tool_requires_graph(name: &str) -> bool {
        matches!(
            name,
            "symbol"
                | "neighbors"
                | "callers"
                | "callees"
                | "callgraph"
                | "path"
                | "explore"
                | "impact"
                | "context"
        )
    }

    /// Return whether this concrete tool call needs the graph before dispatch.
    ///
    /// Background-capable tools must not perform expensive graph construction in
    /// the foreground before returning their `task_id`.
    pub fn tool_call_requires_graph(name: &str, _arguments: &Value) -> bool {
        Self::tool_requires_graph(name)
    }

    /// Build the graph engine on first use.
    /// This is called only for graph-backed tool calls after the MCP handshake
    /// completes, so the client doesn't timeout waiting for a startup response.
    pub fn ensure_graph_initialized(&mut self) -> anyhow::Result<()> {
        if self.graph_initialized {
            return Ok(());
        }
        tracing::info!("Building graph snapshot (first request)...");
        let graph = Arc::new(atlas_engine::GraphEngine::from_store(&self.store, 0.3)?);
        self.search = Some(SearchEngine::new(
            Arc::clone(&self.store),
            Arc::clone(&graph),
        ));
        self.context = Some(
            ContextBuilder::new(Arc::clone(&self.store), graph)
                .with_project_root(self.project_root.clone()),
        );
        self.last_graph_signature = self.store.index_signature().unwrap_or_default();
        self.graph_initialized = true;
        tracing::info!("Graph snapshot ready.");
        Ok(())
    }

    /// Access the search engine (panics if graph not initialized).
    pub(crate) fn search_engine(&self) -> &SearchEngine {
        self.search.as_ref().expect("graph not initialized")
    }

    /// Access the context builder (panics if graph not initialized).
    pub(crate) fn context_builder(&self) -> &ContextBuilder {
        self.context.as_ref().expect("graph not initialized")
    }

    /// Check if the store has any indexed files (fast COUNT query).
    pub(crate) fn has_indexed_files(&self) -> bool {
        self.store.count_files().unwrap_or(0) > 0
    }

    /// Return a guidance string when the project has not been indexed yet.
    pub(crate) fn index_not_run_guidance(&self) -> &'static str {
        if !self.has_indexed_files() {
            "\nHint: The project has not been indexed yet. Please run the 'index' tool first (fast manifest indexing) to build the code index, then retry this query."
        } else {
            ""
        }
    }

    /// Resolve a [`FileId`] to its human-readable file path.
    /// Falls back to the hex representation if the file is not found.
    pub(crate) fn resolve_file_path(&self, file_id: &FileId) -> String {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|f| f.path)
            .unwrap_or_else(|| file_id.to_hex())
    }

    /// Switch the active project to a new store+root, clearing graph/cache state.
    ///
    /// This is the core mechanism for `atlas_open_project` and project switching.
    /// After activation, the next graph-backed tool call will lazily rebuild the
    /// snapshot from the new store.
    pub(crate) fn activate_project(&mut self, project_root: std::path::PathBuf, store: Arc<Store>) {
        self.project_root = project_root.clone();
        self.store = store.clone();
        self.lazy_service = LazyDataflowService::new(store, Some(project_root));
        self.search = None;
        self.context = None;
        self.graph_initialized = false;
        self.cached_signature.clear();
        self.last_graph_signature.clear();
        self.last_signature_check = std::time::Instant::now();
    }

    /// Activate a prepared background `open_project` result, if one exists.
    pub(crate) fn activate_pending_project_for_task(&mut self, task_id: &str) -> Option<String> {
        let pending = self
            .pending_project_activations
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(task_id));
        pending.map(|activation| {
            let project = activation.project_root.display().to_string();
            self.activate_project(activation.project_root, activation.store);
            project
        })
    }

    /// Refresh the graph snapshot if an external index/sync has changed the DB.
    /// Signature is cached for 5 seconds to avoid per-request COUNT queries.
    pub(crate) fn maybe_refresh_graph(&mut self) -> anyhow::Result<()> {
        if !self.graph_initialized {
            return Ok(());
        }
        // Cache signature for 5 seconds
        if self.last_signature_check.elapsed().as_secs() < 5 {
            return Ok(());
        }
        self.last_signature_check = std::time::Instant::now();
        let current = self
            .store
            .index_signature()
            .unwrap_or_else(|_| self.cached_signature.clone());
        if current != self.last_graph_signature {
            tracing::info!("Index signature changed, refreshing graph");
            let graph = Arc::new(atlas_engine::GraphEngine::from_store(&self.store, 0.3)?);
            if let Some(ref mut s) = self.search {
                s.refresh_graph(Arc::clone(&graph));
            }
            if let Some(ref mut c) = self.context {
                c.refresh_graph(graph);
            }
            self.last_graph_signature = current.clone();
        }
        self.cached_signature = current;
        Ok(())
    }

    /// Handle tools/list — return all registered tool definitions.
    pub fn list_tools(&self) -> ListToolsResult {
        ListToolsResult {
            tools: self.tools.clone(),
        }
    }

    /// Handle tools/call — dispatch by tool name.
    ///
    /// Graph initialization and signature-refresh are handled by the MCP
    /// server layer ([`AtlasMcpService::call_tool`]) before this method is
    /// called. The dispatcher itself only routes to handlers.
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> CallToolResult {
        // No per-request graph rebuild — engines were initialized at startup.
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let (result, is_error) = match name {
            "index" => self.handle_index(arguments),
            "open_project" => self.handle_open_project(arguments),
            "status" => self.handle_status(),
            "files" => self.handle_files(),
            "search" => self.handle_search(arguments),
            "symbol" => self.handle_symbol(arguments),
            "neighbors" => self.handle_neighbors(arguments),
            "callers" => self.handle_callers(arguments),
            "callees" => self.handle_callees(arguments),
            "callgraph" => self.handle_callgraph(arguments),
            "path" => self.handle_path(arguments),
            "explore" => self.handle_explore(arguments),
            "impact" => self.handle_impact(arguments),
            "context" => self.handle_context(arguments),
            "trace_point" => self.handle_trace_point(arguments),
            "trace_variable" => self.handle_trace_variable(arguments),
            "trace_caller_path" => self.handle_trace_caller_path(arguments),
            "trace_forward" => self.handle_trace_forward(arguments),
            "language_capabilities" => self.handle_language_capabilities(),
            "usages" => self.handle_usages(arguments),
            "dependencies" => self.handle_dependencies(arguments),
            "dependents" => self.handle_dependents(arguments),
            "task_status" => self.handle_task_status(arguments),
            "wait_for_task" => self.handle_wait_for_task(arguments),
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

    pub(crate) fn handle_task_status(&mut self, args: &serde_json::Value) -> (String, bool) {
        let task_id = get_str(args, "task_id");
        if task_id.is_empty() {
            return ("Missing task_id parameter".to_string(), true);
        }
        match self.task_manager.get_task(task_id) {
            Some(info) => {
                let status_str = match info.status {
                    crate::task_manager::TaskStatus::Running => "running",
                    crate::task_manager::TaskStatus::Completed => "completed",
                    crate::task_manager::TaskStatus::Failed => "failed",
                };
                let mut response = json!({
                    "task_id": info.task_id,
                    "tool_name": info.tool_name,
                    "method": info.method,
                    "status": status_str,
                    "progress": info.progress,
                    "progress_message": info.progress_message,
                    "elapsed_secs": info.elapsed_secs(),
                });
                if let Some(ref result) = info.result {
                    response["result"] = result.clone();
                }
                if let Some(ref error) = info.error {
                    response["error"] = serde_json::Value::String(error.clone());
                }
                if status_str == "completed" && info.method == "open_project" {
                    if let Some(project) = self.activate_pending_project_for_task(&info.task_id) {
                        response["activation"] = serde_json::Value::String("activated".into());
                        response["activated_project"] = serde_json::Value::String(project);
                    } else {
                        response["activation"] =
                            serde_json::Value::String("already_activated".into());
                    }
                }
                (
                    serde_json::to_string_pretty(&response).unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            None => (format!("Task not found: {}", task_id), true),
        }
    }

    // -------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------

    /// Resolve a qualified name to a SymbolId, returning error string on failure.
    /// When the store has no indexed files, the error includes guidance to run `index`.
    pub(crate) fn resolve_qname(&self, qname: &str) -> Result<SymbolId, String> {
        let symbols = self
            .store
            .find_symbols_by_qname(qname)
            .map_err(|e| format!("Lookup error: {}", e))?;
        match symbols.first() {
            Some(s) => Ok(s.id),
            None => {
                let mut err = format!("Symbol not found: {}", qname);
                err.push_str(self.index_not_run_guidance());
                Err(err)
            }
        }
    }

    /// Render a node from the graph snapshot to JSON.
    pub(crate) fn node_json(snap: &atlas_engine::GraphSnapshot, ix: atlas_engine::NodeIx) -> Value {
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
            name: "index".into(),
            description: "Index/re-index the active project for MCP use. This tool always performs fast manifest indexing (files plus basic symbols/functions); deeper structural parsing happens through scoped search/trace on demand. Use background=true + wait_for_task for very large projects. Parameters: include/exclude glob patterns, background (default false).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "include": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to restrict indexing to specific directories/files (e.g. [\"src/**\"])" },
                    "exclude": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns for directories/files to skip (e.g. [\"**/test/**\", \"**/*.spec.ts\"])" },
                    "background": { "type": "boolean", "description": "Run indexing as a background task (returns task_id for task_status/wait_for_task)" },
                })),
                required: None,
            },
        },
        Tool {
            name: "open_project".into(),
            description: "Open and activate a project only. This tool never indexes. After activation, call index to index the active project, then search with a required scope. Defaults to storage=\"memory\". Parameters: project_path (required), storage, scan_files, background.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "project_path": { "type": "string", "description": "Absolute path to the project directory to open" },
                    "storage": { "type": "string", "enum": ["memory", "persistent"], "description": "Storage mode: \"memory\" (in-memory, zero footprint, default) or \"persistent\" (project/.atlas/atlas.db)" },
                    "scan_files": { "type": "boolean", "description": "Run file discovery to estimate file_count without indexing (default false; can be slow on very large trees)" },
                    "background": { "type": "boolean", "description": "Prepare/open in a background task; task_status/wait_for_task activates the completed project" },
                })),
                required: Some(vec!["project_path".into()]),
            },
        },
        Tool {
            name: "status".into(),
            description: "Show project overview: file/symbol/edge counts, DB stats, per-language capability profiles.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "files".into(),
            description: "List all indexed files with language and parse status.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "search".into(),
            description: "Search symbols by name within a required project-relative scope. Small scopes are structurally parsed for precise function search; large scopes stay manifest-level and return a warning to narrow scope. Without scope, the tool returns an error and does not extract or run follow-up parsing. Supports kind filter and background=true.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query": { "type": "string", "description": "Search query text" },
                    "scope": { "type": "string", "description": "Required project-relative directory or file scope (e.g. 'drivers/net', 'src', 'kernel/sched'). Use 'files' to discover indexed paths." },
                    "kind": { "type": "string", "description": "Optional SymbolKind filter (function, class, ...)" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                    "background": { "type": "boolean", "description": "Run search as background task (returns task_id for task_status polling)" },
                })),
                required: Some(vec!["query".into(), "scope".into()]),
            },
        },
        Tool {
            name: "symbol".into(),
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
            name: "neighbors".into(),
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
            name: "callers".into(),
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
            name: "callees".into(),
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
            name: "callgraph".into(),
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
            name: "path".into(),
            description: "Find the shortest path between two symbols through the graph (BFS). By default only follows call edges (calls, instantiates, implements, registers_callback). Use edge_kinds to override.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "from": { "type": "string", "description": "Source symbol qualified name" },
                    "to": { "type": "string", "description": "Target symbol qualified name" },
                    "max_depth": { "type": "integer", "description": "Max search depth (default 5, max 10)" },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\", \"instantiates\", \"implements\", \"registers_callback\"]. Use [] or [\"*\"] for all edge kinds."
                    },
                })),
                required: Some(vec!["from".into(), "to".into()]),
            },
        },
        Tool {
            name: "explore".into(),
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
            name: "impact".into(),
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
            name: "context".into(),
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
            name: "trace_point".into(),
            description: "Resolve a source position (file_id or file_path + line + column) to its full context: reference, symbol, data node, scope, bindings, and incident dataflow edges.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex (from atlas_files)" },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts')" },
                    "line": { "type": "integer", "description": "1-based line number" },
                    "column": { "type": "integer", "description": "1-based column number" },
                })),
                required: Some(vec!["line".into(), "column".into()]),
            },
        },
        Tool {
            name: "trace_variable".into(),
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
                required: Some(vec!["line".into(), "column".into()]),
            },
        },
        Tool {
            name: "trace_caller_path".into(),
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
            name: "trace_forward".into(),
            description: "Trace the forward call chain from source to target. Answers 'how does A reach B?' by walking forward through call edges. Returns per-hop source snippets and edge types.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "from": { "type": "string", "description": "Source symbol ID in hex" },
                    "to": { "type": "string", "description": "Target symbol ID in hex" },
                    "max_depth": { "type": "integer", "description": "Maximum forward call depth (default 10)" },
                })),
                required: Some(vec!["from".into(), "to".into()]),
            },
        },
        Tool {
            name: "language_capabilities".into(),
            description: "Show per-language analysis capability profiles: supported features, limitations, confidence floor.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "usages".into(),
            description: "Find all reference usages of a symbol (where it's called, referenced, or instantiated).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "limit": { "type": "integer", "description": "Max results (default 50)" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "dependencies".into(),
            description: "Find files that a given file imports or includes (outgoing dependencies).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex format" },
                    "limit": { "type": "integer", "description": "Max results (default 50)" },
                })),
                required: Some(vec!["file_id".into()]),
            },
        },
        Tool {
            name: "dependents".into(),
            description: "Find files that import or include a given file (incoming dependents / reverse dependencies).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex format" },
                    "limit": { "type": "integer", "description": "Max results (default 50)" },
                })),
                required: Some(vec!["file_id".into()]),
            },
        },
        Tool {
            name: "task_status".into(),
            description: "Poll the status of a background task. This is the preferred progress path for clients that do not support MCP progress notifications. Returns running/completed/failed, progress percentage, progress_message, and result when complete.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "task_id": { "type": "string", "description": "Task ID returned by a tool when background=true" },
                })),
                required: Some(vec!["task_id".into()]),
            },
        },
        Tool {
            name: "wait_for_task".into(),
            description: "Block until a background task completes or timeout_secs elapses. Prefer task_status polling for clients with short tool-call timeouts. Parameters: task_id (required), timeout_secs (default 30, max 300; 0 means single status check), poll_interval_secs (default 2).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "task_id": { "type": "string", "description": "Task ID from a background=true response" },
                    "timeout_secs": { "type": "integer", "description": "Max seconds to wait (default 30, max 300)" },
                    "poll_interval_secs": { "type": "integer", "description": "Seconds between polls (default 2, 1-10)" },
                })),
                required: Some(vec!["task_id".into()]),
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
    if s.len() <= max_len {
        return s;
    }
    let mut end = 0;
    for (idx, _) in s.char_indices() {
        if idx > max_len {
            break;
        }
        end = idx;
    }
    &s[..end]
}
