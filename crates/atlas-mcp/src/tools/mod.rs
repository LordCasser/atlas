//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, lifecycle, branch_diff.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use atlas_engine::ContextBuilder;
use atlas_engine::FileId;
use atlas_engine::LazyDataflowService;
use atlas_engine::LazyOrchestrator;
use atlas_engine::LazyPolicy;
use atlas_engine::SearchEngine;
use atlas_engine::SourceExtractor;
use atlas_engine::Store;
use atlas_engine::SymbolId;
use atlas_engine::TraceDiagnostic;

use super::protocol::{CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema};

use serde_json::{Value, json};

use crate::tools::async_state::AsyncState;
use crate::tools::cache_state::CacheState;
use crate::tools::lazy_response::{CapabilityStats, SnapshotStore};
use symbol_selector::{parse_symbol_input, SymbolInput};
use crate::tools::query_snapshot::{InvestigationState, QuerySnapshot};
use crate::tools::graph_state::GraphState;

/// Progress report tuple: (progress, total, message)
pub(crate) type ProgressReport = (f64, Option<f64>, Option<String>);
/// Channel sender for progress updates during long-running operations.
pub(crate) type ProgressSender = tokio::sync::mpsc::UnboundedSender<ProgressReport>;

/// Maximum number of ambiguous candidates to display in diagnostics.
/// Beyond this, candidates are truncated to avoid log flooding in large projects.
pub(crate) const MAX_AMBIGUOUS_CANDIDATES: usize = 5;

// -------------------------------------------------------------------
// ToolCallContext — request-scoped progress capabilities
// -------------------------------------------------------------------

/// Request-scoped context for a single tool call.
///
/// Carries the progress sender, task manager, and task id so that handlers
/// do not rely on global mutable state on [`ToolRouter`].
#[derive(Clone)]
pub struct ToolCallContext {
    /// MCP progress notification sender (None = no progress token).
    pub progress_sender: Option<ProgressSender>,
    /// Task manager for background task progress (None = foreground).
    pub task_manager: Option<std::sync::Arc<crate::task_manager::TaskManager>>,
    /// Task id for background task progress (None = foreground).
    pub task_id: Option<String>,
}

impl ToolCallContext {
    /// Create a context with no progress capabilities.
    pub fn empty() -> Self {
        Self {
            progress_sender: None,
            task_manager: None,
            task_id: None,
        }
    }

    /// Create a context from a progress sender.
    pub fn with_progress_sender(sender: ProgressSender) -> Self {
        Self {
            progress_sender: Some(sender),
            task_manager: None,
            task_id: None,
        }
    }

    /// Create a context for background tasks (task-manager-based progress).
    pub fn with_task_manager(
        task_manager: std::sync::Arc<crate::task_manager::TaskManager>,
        task_id: String,
    ) -> Self {
        Self {
            progress_sender: None,
            task_manager: Some(task_manager),
            task_id: Some(task_id),
        }
    }

    /// Send progress via the MCP channel if a progress_sender is configured.
    ///
    /// This is a no-op when no progress token was provided by the client.
    pub fn send_progress(&self, fraction: f64, message: &str) {
        if let Some(ref sender) = self.progress_sender {
            let _ = sender.send((fraction, None, Some(message.to_string())));
        }
    }
}

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

// -------------------------------------------------------------------
// Input length bounds — protect against malicious oversized inputs
// -------------------------------------------------------------------

/// Maximum length of a search query string.
pub(crate) const MAX_QUERY_LENGTH: usize = 1024;
/// Maximum length of a symbol name / qualified name.
pub(crate) const MAX_SYMBOL_NAME_LENGTH: usize = 512;
/// Maximum length of an annotation qualified name (field_qname / target_qname).
pub(crate) const MAX_ANNOTATION_QNAME_LENGTH: usize = 512;
/// Maximum length of a file path.
pub(crate) const MAX_FILE_PATH_LENGTH: usize = 4096;

pub(crate) mod annotations;
pub(crate) mod async_state;
pub(crate) mod atlas_jobs;
pub(crate) mod branch_diff;
pub(crate) mod cache_state;
pub(crate) mod context;
pub(crate) mod dependencies;
pub(crate) mod dependents;
pub(crate) mod domain_rules;
pub(crate) mod graph;
pub(crate) mod graph_state;
pub(crate) mod index;
pub(crate) mod lazy_refresh;
pub(crate) mod lazy_response;
pub(crate) mod lifecycle;
pub(crate) mod open_project;
pub(crate) mod query_snapshot;
pub(crate) mod resume;
pub(crate) mod search;
pub(crate) mod status;
pub(crate) mod symbol_selector;
pub(crate) mod trace;
pub(crate) mod usages;
pub(crate) mod wait_for;

// -------------------------------------------------------------------
// StructuralEnsureOutcome
// -------------------------------------------------------------------

/// Result of lazy structural extraction triggered via
/// [`ensure_structural_for_files`] or [`ensure_structural_for_symbol_name`].
pub(crate) struct StructuralEnsureOutcome {
    pub warnings: Vec<String>,
    pub built_file_ids: Vec<atlas_engine::FileId>,
    pub precision_tier: atlas_engine::structs::precision::PrecisionTier,
    /// Full lazy outcome when extraction actually ran (None if skipped,
    /// e.g., when a manual full index already exists).
    pub lazy_outcome: Option<atlas_engine::LazyOutcome>,
}

// -------------------------------------------------------------------
// ToolRouter
// -------------------------------------------------------------------

/// Dispatches tools/list and tools/call.
pub struct ToolRouter {
    pub(crate) store: Arc<Store>,
    /// High-level Engine wrapping the full extraction → trace pipeline.
    /// Wrapped in Mutex because Engine contains RefCell (Send but not Sync).
    pub(crate) engine: Mutex<atlas_engine::Engine>,
    pub(crate) lazy_service: LazyDataflowService,
    /// Graph engines and lifecycle state (lazy init, refresh, background rebuild).
    pub(crate) graph: GraphState,
    /// Index-signature and manual-full-index caching.
    pub(crate) cache: CacheState,
    /// AST-aware source extractor (tree-sitter re-parsing).
    pub(crate) source_extractor: SourceExtractor,
    /// Project root directory for snippet extraction.
    pub(crate) project_root: std::path::PathBuf,
    tools: Vec<Tool>,
    /// Consolidates lazy graph refresh state: pending file IDs, cumulative
    /// write counter, and deferred full-rebuild scheduling.
    pub(crate) lazy_refresh_queue: Arc<lazy_refresh::LazyRefreshQueue>,
    /// Background task and async operation state.
    pub(crate) async_state: AsyncState,
    /// Investigation state (MCP session scoped) for lazy job prioritization.
    pub(crate) investigation_state: InvestigationState,
}

impl ToolRouter {
    fn project_runtime(
        store: Arc<Store>,
        project_root: &std::path::Path,
    ) -> (atlas_engine::Engine, LazyDataflowService, SourceExtractor) {
        (
            atlas_engine::Engine::from_store(store.clone(), Some(project_root)),
            LazyDataflowService::new(store.clone(), Some(project_root.to_path_buf())),
            SourceExtractor::new(store, project_root.to_path_buf()),
        )
    }

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
        let (engine, lazy_service, source_extractor) =
            Self::project_runtime(store.clone(), &project_root);
        Self {
            store: store.clone(),
            engine: Mutex::new(engine),
            lazy_service,
            graph: GraphState {
                search: Some(search),
                context: Some(context),
                graph_initialized: true,
                last_graph_signature: last_graph_signature.clone(),
                pending_graph_rebuild: Arc::new(Mutex::new(None)),
            },
            source_extractor,
            project_root,
            tools: make_all_tools(),
            lazy_refresh_queue: lazy_refresh::LazyRefreshQueue::new(),
            cache: CacheState {
                cached_signature: last_graph_signature.clone(),
                last_signature_check: std::time::Instant::now(),
                cached_manual_full_index: RwLock::new(None),
            },
            async_state: AsyncState {
                task_manager: Arc::new(crate::task_manager::TaskManager::new()),
                pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
                query_snapshots: Mutex::new(HashMap::new()),
                prewarm_running: Arc::new(AtomicBool::new(false)),
            },
            investigation_state: InvestigationState::default(),
        }
    }

    /// Create a router without building the graph (fast startup).
    /// Graph is built lazily on the first request via `ensure_graph_initialized`.
    pub fn new_empty(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        let tools = make_all_tools();
        let (engine, lazy_service, source_extractor) =
            Self::project_runtime(store.clone(), &project_root);
        Self {
            store: store.clone(),
            engine: Mutex::new(engine),
            lazy_service,
            graph: GraphState {
                search: None,
                context: None,
                graph_initialized: false,
                last_graph_signature: String::new(),
                pending_graph_rebuild: Arc::new(Mutex::new(None)),
            },
            source_extractor,
            project_root,
            tools,
            lazy_refresh_queue: lazy_refresh::LazyRefreshQueue::new(),
            cache: CacheState {
                cached_signature: String::new(),
                last_signature_check: std::time::Instant::now(),
                cached_manual_full_index: RwLock::new(None),
            },
            async_state: AsyncState {
                task_manager: Arc::new(crate::task_manager::TaskManager::new()),
                pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
                query_snapshots: Mutex::new(HashMap::new()),
                prewarm_running: Arc::new(AtomicBool::new(false)),
            },
            investigation_state: InvestigationState::default(),
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
        matches!(name, "symbol" | "calls" | "path" | "explore" | "impact")
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
        self.graph.ensure_initialized(&self.store, &self.source_extractor, &self.project_root)
    }

    /// Access the search engine.
    pub(crate) fn search_engine(&self) -> Result<&SearchEngine, graph_state::GraphNotInitializedError> {
        self.graph.search_engine()
    }

    /// Access the context builder.
    pub(crate) fn context_builder(&self) -> Result<&ContextBuilder, graph_state::GraphNotInitializedError> {
        self.graph.context_builder()
    }

    /// Check if the store has any indexed files (fast COUNT query).
    pub(crate) fn has_indexed_files(&self) -> bool {
        self.store.count_files().unwrap_or(0) > 0
    }

    /// Query the DB for real capability file counts.
    /// Returns None if the query fails (graceful degradation).
    pub(crate) fn get_capability_stats(&self) -> Option<CapabilityStats> {
        let (files_with_dataflow, files_structural_only, files_manifest_only, files_with_cfg) =
            self.store.get_capability_counts().ok()?;
        Some(CapabilityStats {
            files_with_dataflow,
            files_structural_only,
            files_manifest_only,
            files_with_cfg,
        })
    }

    /// Return a guidance string when the project has not been indexed yet.
    pub(crate) fn index_not_run_guidance(&self) -> &'static str {
        if !self.has_indexed_files() {
            "\nHint: The project has not been indexed yet. Please run the 'index' tool first (fast manifest indexing) to build the code index, then retry this query."
        } else {
            ""
        }
    }

    /// Rebuild the graph snapshot from the store if the index signature changed.
    fn rebuild_if_signature_changed(&mut self, reason: &str) -> anyhow::Result<()> {
        let current = self
            .store
            .index_signature()
            .unwrap_or_else(|_| self.cache.cached_signature.clone());
        if current != self.graph.last_graph_signature {
            tracing::info!("{reason}");
            let graph = Arc::new(atlas_engine::GraphEngine::from_store(&self.store, 0.3)?);
            if let Some(ref mut s) = self.graph.search {
                s.refresh_graph(Arc::clone(&graph));
            }
            if let Some(ref mut c) = self.graph.context {
                c.refresh_graph(graph);
            }
            self.graph.last_graph_signature = current.clone();
            // Re-check whether a manual full index now exists (layer distribution
            // may have changed after external index/sync or lazy structural).
            *self.cache.cached_manual_full_index.write().expect("cached_manual_full_index lock poisoned") = None;
        }
        self.cache.cached_signature = current;
        Ok(())
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
        self.lazy_refresh_queue.clear();
        self.lazy_refresh_queue = lazy_refresh::LazyRefreshQueue::new();
        self.graph.pending_graph_rebuild = Arc::new(Mutex::new(None));

        let (engine, lazy_service, source_extractor) =
            Self::project_runtime(store.clone(), &project_root);
        self.project_root = project_root.clone();
        self.store = store.clone();
        *self.engine.lock().expect("engine lock poisoned") = engine;
        self.lazy_service = lazy_service;
        self.source_extractor = source_extractor;
        self.graph.search = None;
        self.graph.context = None;
        self.graph.graph_initialized = false;
        self.cache.cached_signature.clear();
        self.graph.last_graph_signature.clear();
        self.cache.last_signature_check = std::time::Instant::now();
        *self.cache.cached_manual_full_index.write().expect("cached_manual_full_index lock poisoned") = None;
        self.async_state.query_snapshots.lock().expect("query_snapshots lock poisoned").clear();
        self.investigation_state = InvestigationState::default();
    }

    /// Activate a prepared background `open_project` result, if one exists.
    pub(crate) fn activate_pending_project_for_task(&mut self, task_id: &str) -> Option<String> {
        let pending = self
            .async_state.pending_project_activations
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(task_id));
        pending.map(|activation| {
            let project = activation.project_root.display().to_string();
            self.activate_project(activation.project_root, activation.store);
            project
        })
    }

    /// Ensure the in-memory call-graph reflects any newly extracted structural data.
    ///
    /// **Refresh responsibility**: this method is already called internally by
    /// [`ensure_structural_for_files`] and [`ensure_structural_for_symbol_name`]
    /// whenever they actually built new files.  Callers that use those helpers
    /// do **not** need to call `maybe_refresh_graph` separately.
    ///
    /// Callers that modify the store independently (e.g. through a full re-index
    /// signal) may still need to call this to pick up changes.
    pub(crate) fn maybe_refresh_graph(&mut self) -> anyhow::Result<()> {
        if !self.graph.graph_initialized {
            return Ok(());
        }

        // Step 1: Always flush pending incremental writes (no cooldown).
        // This ensures lazy writes from THIS request are visible before graph queries.
        let batch = self.lazy_refresh_queue.take_incremental_batch(500);
        self.graph.refresh_graph_for_files(&self.store, &batch)?;
        // Cache invalidation: new store data may have changed layer distribution.
        if !batch.is_empty() {
            *self.cache.cached_manual_full_index.write().expect("cached_manual_full_index lock poisoned") = None;
        }

        // Step 2: Deferred full rebuild — try to apply a background-built graph,
        // or spawn the rebuild thread. NEVER blocks the current request.
        self.graph.try_apply_or_spawn_rebuild(
            Arc::clone(&self.store),
            Arc::clone(&self.lazy_refresh_queue),
        );

        // Step 3: Always check the store signature. A full index may change
        // extraction layers and graph facts without going through this router.
        self.cache.last_signature_check = std::time::Instant::now();
        self.rebuild_if_signature_changed("Index signature changed, refreshing graph")
    }

    /// Force-refresh the graph snapshot regardless of cache cooldown.
    ///
    /// Called after lazy structural extraction writes new facts to the DB
    /// (via the context tool's tier-3 symbol resolution), so that the
    /// in-memory graph includes the newly parsed edges before graph-backed
    /// tools run their queries.
    pub(crate) fn force_refresh_graph(&mut self) -> anyhow::Result<()> {
        if !self.graph.graph_initialized {
            return Ok(());
        }
        self.cache.last_signature_check = std::time::Instant::now();
        self.rebuild_if_signature_changed("Force-refreshing graph after lazy structural extraction")
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
    pub fn call_tool(
        &mut self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: &Value,
    ) -> CallToolResult {
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let (result, is_error) = match name {
            "project" => self.handle_project(arguments),
            "index" => self.handle_index(ctx, arguments),
            "search" => self.handle_search(ctx, arguments),
            "symbol" => self.handle_symbol(ctx, arguments),
            "calls" => self.handle_calls(arguments),
            "explore" => self.handle_explore(arguments),
            "path" => self.handle_path(arguments),
            "impact" => self.handle_impact(arguments),
            "file_dependencies" => self.handle_file_dependencies(arguments),
            "trace" => self.handle_trace(ctx, arguments),
            "lifecycle" => self.handle_lifecycle(arguments),
            "branch_diff" => self.handle_branch_diff(arguments),
            "fp_dispatches" => self.handle_fp_dispatches(arguments),
            "domain_rules" => self.handle_domain_rules(arguments),
            "tasks" => self.handle_tasks(arguments),
            "task_status" => self.handle_task_status(arguments),
            "wait_for_task" => self.handle_wait_for_task_sync(arguments),
            "resume_task" => self.handle_resume_task(arguments),
            _ => (format!("Unknown tool: {name}"), true),
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
        match self.async_state.task_manager.get_task(task_id) {
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
                if status_str == "completed" && info.method == "project" {
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
            None => (format!("Task not found: {task_id}"), true),
        }
    }

    /// Parse and validate `include_roots` from MCP arguments.
    /// Returns project-relative roots and any validation warnings.
    pub(crate) fn include_roots_from_args(
        &self,
        args: &serde_json::Value,
    ) -> (Vec<atlas_engine::IncludeRoot>, Vec<String>) {
        let mut roots = Vec::new();
        let mut warnings = Vec::new();

        let arr = match args.get("include_roots") {
            Some(serde_json::Value::Array(a)) => a,
            Some(_) => {
                warnings.push("include_roots must be an array of strings".into());
                return (roots, warnings);
            }
            None => return (roots, warnings),
        };

        const MAX_ROOTS: usize = 16;
        if arr.len() > MAX_ROOTS {
            warnings.push(format!(
                "include_roots: truncated from {} to {MAX_ROOTS} entries",
                arr.len()
            ));
        }

        let mut seen: HashSet<String> = HashSet::new();
        for item in arr.iter().take(MAX_ROOTS) {
            let raw = match item.as_str() {
                Some(s) => s,
                None => {
                    warnings.push("include_roots: non-string entry skipped".into());
                    continue;
                }
            };

            // Validate
            if raw.len() > 256 {
                warnings.push(format!("include_roots: path too long (>256): {raw}"));
                continue;
            }
            if raw.starts_with('/') || raw.starts_with('\\') {
                warnings.push(format!("include_roots: absolute path rejected: {raw}"));
                continue;
            }
            // Reject Windows drive-letter paths (C:\foo → C:/foo is still absolute)
            let is_drive_letter = raw.len() >= 2
                && raw.as_bytes()[0].is_ascii_alphabetic()
                && raw.as_bytes()[1] == b':';
            if is_drive_letter {
                warnings.push(format!("include_roots: absolute path rejected: {raw}"));
                continue;
            }

            // Normalize
            let normalized = match normalize_project_relative_path(raw) {
                Some(p) => p,
                None => {
                    warnings.push(format!("include_roots: path escapes project: {raw}"));
                    continue;
                }
            };

            if normalized.is_empty() || normalized == "." {
                warnings.push(format!("include_roots: empty path after normalize: {raw}"));
                continue;
            }

            // Warn if directory doesn't exist (non-fatal)
            if !self.project_root.join(&normalized).is_dir() {
                warnings.push(format!(
                    "include_roots: directory not found (used anyway): {normalized}"
                ));
            }

            if seen.insert(normalized.clone()) {
                roots.push(atlas_engine::IncludeRoot { path: normalized });
            }
        }

        (roots, warnings)
    }

    // -------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------

    /// Ensure structural data for the given files, with optional
    /// include_roots for C/C++ angle-bracket resolution.
    /// Returns outcome with warnings and built file IDs (the caller should
    /// surface warnings in the MCP response).
    ///
    /// No-op when a manual full index already exists.
    ///
    /// When `investigation` is provided, files related to the investigation
    /// are prioritized before unrelated files.
    /// When `query_id` is provided, it is threaded into extraction job records
    /// so `atlas_jobs` can filter by query.
    /// After this returns, the in-memory graph is guaranteed to be refreshed
    /// if any new files were built. Callers do not need to call
    /// `maybe_refresh_graph()` separately.
    pub(crate) fn ensure_structural_for_files(
        &mut self,
        file_ids: impl IntoIterator<Item = FileId>,
        include_roots: Vec<atlas_engine::IncludeRoot>,
        investigation: Option<&atlas_engine::Investigation>,
        query_id: Option<&str>,
    ) -> StructuralEnsureOutcome {
        use atlas_engine::structs::precision::PrecisionTier;

        let mut warnings = Vec::new();
        let mut built_file_ids = Vec::new();
        if self.cache.has_manual_full_index(&self.store) {
            return StructuralEnsureOutcome {
                warnings,
                built_file_ids,
                precision_tier: PrecisionTier::Exact,
                lazy_outcome: None,
            };
        }
        // Deduplicate
        let file_set: HashSet<_> = file_ids.into_iter().collect();
        if file_set.is_empty() {
            return StructuralEnsureOutcome {
                warnings,
                built_file_ids,
                precision_tier: PrecisionTier::Exact,
                lazy_outcome: None,
            };
        }
        let file_vec: Vec<FileId> = file_set.into_iter().collect();
        let orchestrator = LazyOrchestrator::new(
            self.store.clone(),
            Some(self.project_root.clone()),
            include_roots,
        )
        .with_prewarm_flag(self.async_state.prewarm_running.clone());
        let outcome = match orchestrator.ensure_structural_for_files(
            &file_vec,
            LazyPolicy::ForegroundStructural,
            investigation,
            query_id,
        ) {
            Ok(o) => o,
            Err(e) => {
                warnings.push(format!("Lazy structural extraction failed: {e:#}"));
                return StructuralEnsureOutcome {
                    warnings,
                    built_file_ids,
                    precision_tier: PrecisionTier::Unavailable,
                    lazy_outcome: None,
                };
            }
        };
        // Clone before field moves so we can pass it through to handlers.
        let lazy_outcome = outcome.clone();
        self.lazy_refresh_queue
            .record_lazy_writes(&outcome.built_file_ids);
        built_file_ids = outcome.built_file_ids;
        if !built_file_ids.is_empty() {
            if let Err(e) = self.maybe_refresh_graph() {
                warnings.push(format!("Graph refresh failed: {e:#}"));
            }
        }
        StructuralEnsureOutcome {
            warnings,
            built_file_ids,
            precision_tier: outcome.precision_tier,
            lazy_outcome: Some(lazy_outcome),
        }
    }

    /// Ensure structural data for files containing a symbol name,
    /// with optional include_roots.  Useful for name-based lookups
    /// where the file_id is not yet known.
    /// When `query_id` is provided, it is threaded into extraction job records.
    /// After this returns, the in-memory graph is guaranteed to be refreshed
    /// if any new files were built. Callers do not need to call
    /// `maybe_refresh_graph()` separately.
    pub(crate) fn ensure_structural_for_symbol_name(
        &mut self,
        symbol_name: &str,
        include_roots: Vec<atlas_engine::IncludeRoot>,
        investigation: Option<&atlas_engine::Investigation>,
        query_id: Option<&str>,
    ) -> StructuralEnsureOutcome {
        use atlas_engine::structs::precision::PrecisionTier;

        let mut warnings = Vec::new();
        let mut built_file_ids = Vec::new();
        if self.cache.has_manual_full_index(&self.store) {
            return StructuralEnsureOutcome {
                warnings,
                built_file_ids,
                precision_tier: PrecisionTier::Exact,
                lazy_outcome: None,
            };
        }
        let orchestrator = LazyOrchestrator::new(
            self.store.clone(),
            Some(self.project_root.clone()),
            include_roots,
        )
        .with_prewarm_flag(self.async_state.prewarm_running.clone());
        let outcome = match orchestrator.ensure_structural_for_symbol(
            symbol_name,
            LazyPolicy::ForegroundStructural,
            investigation,
            query_id,
        ) {
            Ok(o) => o,
            Err(e) => {
                warnings.push(format!(
                    "Lazy structural extraction failed for '{symbol_name}': {e:#}"
                ));
                return StructuralEnsureOutcome {
                    warnings,
                    built_file_ids,
                    precision_tier: PrecisionTier::Unavailable,
                    lazy_outcome: None,
                };
            }
        };
        // Clone before field moves so we can pass it through to handlers.
        let lazy_outcome = outcome.clone();
        self.lazy_refresh_queue
            .record_lazy_writes(&outcome.built_file_ids);
        built_file_ids = outcome.built_file_ids;
        if !built_file_ids.is_empty() {
            if let Err(e) = self.maybe_refresh_graph() {
                warnings.push(format!("Graph refresh failed: {e:#}"));
            }
        }
        StructuralEnsureOutcome {
            warnings,
            built_file_ids,
            precision_tier: outcome.precision_tier,
            lazy_outcome: Some(lazy_outcome),
        }
    }

    // -------------------------------------------------------------------
    // Query snapshot + investigation helpers
    // -------------------------------------------------------------------
    // Query snapshot + investigation helpers
    // -------------------------------------------------------------------

    /// Generate a time-sortable query_id in format `q_{hex_ts_ms}_{hex_rand4}`.
    /// Uses an atomic counter to prevent collisions within the same millisecond.
    /// Falls back to epoch 0 if system time is before UNIX_EPOCH (practically
    /// impossible but avoids a panic on misconfigured systems).
    pub(crate) fn generate_query_id() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        // XOR ts components to spread bits, then mix with the atomic sequence
        let rand = (((ts >> 10) ^ (ts & 0xFFFF)) as u32 ^ seq ^ (seq.rotate_left(7))) as u16;
        format!("q_{ts:x}_{rand:04x}")
    }

    /// Store a query snapshot, pruning expired entries first.
    ///
    /// Recovers from a poisoned lock (e.g. after a panic in another handler)
    /// rather than panicking — consistent with `AtlasMcpService::lock_router()`.
    pub(crate) fn store_snapshot(&mut self, snapshot: QuerySnapshot) {
        self.async_state.prune_expired_snapshots();
        self.async_state.query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(snapshot.query_id.clone(), snapshot);
    }

    /// Update or create investigation based on a tool call focus.
    pub(crate) fn update_investigation(&mut self, focus: atlas_engine::InvestigationFocus) {
        self.investigation_state.update(focus);
    }

    // -------------------------------------------------------------------
    // Render helpers
    // -------------------------------------------------------------------
    pub(crate) fn node_json(
        &self,
        snap: &atlas_engine::GraphSnapshot,
        ix: atlas_engine::NodeIx,
        edge_kind: Option<&str>,
    ) -> Value {
        let n = snap.node(ix);
        let mut obj = json!({
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
            "file": self.resolve_file_path(&n.file_id),
            "line": n.start_line,
        });
        if let Some(ek) = edge_kind {
            obj["edge"] = json!(ek);
        }
        obj
    }

    /// Read source code for a symbol using AST-aware extraction.
    ///
    /// Delegates to [`SourceExtractor`] which re-parses the file with
    /// tree-sitter and extracts the exact definition-node source text.
    /// Falls back to `TextRange`-based line extraction when tree-sitter
    /// parsing is unavailable.
    ///
    /// Returns `None` if the file cannot be found, is outside the project
    /// root, or the symbol range is invalid.  Callers should silently omit
    /// the `source` field when this returns `None`.
    pub(crate) fn read_symbol_source(&self, symbol_id: &SymbolId) -> Option<String> {
        self.source_extractor.extract_source(symbol_id)
    }
}

// Implement SnapshotStore for ToolRouter so LazyResponse::build() can store
// snapshots without knowing the concrete handler type.
impl SnapshotStore for ToolRouter {
    fn store_query_snapshot(&mut self, snapshot: QuerySnapshot) {
        self.store_snapshot(snapshot);
    }
}

// -------------------------------------------------------------------
// Shared helper functions (module-level, not on ToolRouter)
// -------------------------------------------------------------------

/// Merge root validation warnings and lazy structural warnings into a JSON
/// response's `"warnings"` array. Only adds the key when non-empty.
pub(crate) fn add_json_warnings(
    value: &mut serde_json::Value,
    root_warnings: Vec<String>,
    lazy_warnings: Vec<String>,
) {
    let mut all: Vec<String> = Vec::new();
    all.extend(root_warnings);
    all.extend(lazy_warnings);
    if !all.is_empty() {
        value["warnings"] =
            serde_json::Value::Array(all.into_iter().map(serde_json::Value::String).collect());
    }
}

/// Convert lazy/include_roots warnings into TraceDiagnostics for
/// injection into trace responses.
pub(crate) fn warnings_to_trace_diagnostics(
    warnings: Vec<String>,
    code: &str,
) -> Vec<TraceDiagnostic> {
    warnings
        .into_iter()
        .map(|msg| TraceDiagnostic::warning(&msg).with_code(code))
        .collect()
}

// ===================================================================
// Tool registration — 18 tools (refactored from 33)
// ===================================================================

// ── Project tools ────────────────────────────────────────────────────

fn make_project_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "project".into(),
            description: "Open, inspect, or list files in a project. Use action='open' to activate a project (never indexes), 'status' for a comprehensive overview including language capabilities and index mode, 'files' to list indexed files with language and parse status. Parameters for action='open': project_path (required), storage, force_memory, scan_files, background. If storage is omitted or 'auto', Atlas reuses persistent storage when project status shows a reusable index; otherwise it opens an in-memory project. Explicit storage='memory' is refused when a reusable persistent index exists unless force_memory=true. action='status' returns file/symbol/edge counts, extraction state, per-language capability profiles. action='files' supports optional limit, language, and path_prefix filters.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {
                        "type": "string",
                        "enum": ["open", "status", "files"],
                        "description": "Operation: 'open' activates a project (requires project_path), 'status' shows overview with language capabilities, 'files' lists indexed files."
                    },
                    "project_path": { "type": "string", "description": "Absolute path to the project directory to open (required for action='open')." },
                    "storage": {
                        "type": "string",
                        "enum": ["auto", "memory", "persistent"],
                        "description": "Storage mode: \"auto\" (default; reuse project/.atlas/atlas.db when project status shows a reusable index, otherwise memory), \"memory\" (in-memory, zero footprint; refused if a reusable persistent index exists unless force_memory=true), or \"persistent\" (project/.atlas/atlas.db)."
                    },
                    "force_memory": { "type": "boolean", "description": "Allow storage='memory' even when an existing persistent Atlas index would otherwise be reused. This intentionally starts an empty temporary index." },
                    "scan_files": { "type": "boolean", "description": "Run file discovery to estimate file_count without indexing (default false; can be slow on very large trees)." },
                    "background": { "type": "boolean", "description": "Prepare/open in a background task; task_status/wait_for_task activates the completed project." },
                    "verbose": { "type": "boolean", "description": "Include verbose details (action='status')." },
                    "limit": { "type": "integer", "description": "Max files returned (action='files', default unlimited)." },
                    "language": { "type": "string", "description": "Filter files by language (action='files', e.g. 'rust', 'typescript')." },
                    "path_prefix": { "type": "string", "description": "Filter files by path prefix (action='files')." },
                })),
                required: None,
            },
        },
        Tool {
            name: "index".into(),
            description: "Index/re-index the active project for MCP use. Defaults to fast manifest indexing (files plus basic symbols/functions). If the existing fresh index is structural/full, Atlas refuses lower-precision re-indexing unless force_reindex=true. Pass analysis='structural' for imports/references/call graph, or analysis='full' for dataflow too. Use background=true + wait_for_task for very large projects. Parameters: include/exclude glob patterns, analysis, force_reindex, background (default false).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "include": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to restrict indexing to specific directories/files (e.g. [\"src/**\"])" },
                    "exclude": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns for directories/files to skip (e.g. [\"**/test/**\", \"**/*.spec.ts\"])" },
                    "analysis": { "type": "string", "enum": ["manifest", "structural", "full"], "description": "Index depth. manifest is fast and default; structural adds imports/references/call graph; full also builds dataflow." },
                    "force_reindex": { "type": "boolean", "description": "Allow a lower analysis depth to replace an existing structural/full index. Default false protects manually built rich indexes." },
                    "background": { "type": "boolean", "description": "Run indexing as a background task (returns task_id for task_status/wait_for_task)" },
                })),
                required: None,
            },
        },
    ]
}
// ── SymbolSelector schema helpers ────────────────────────────────────

fn symbol_selector_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Structured symbol selector with fault-tolerant scoring. Fields outside qualified_name are used for ranking only — incorrect values cannot prevent the correct symbol from being found.",
        "required": ["qualified_name"],
        "properties": {
            "qualified_name": {
                "type": "string",
                "description": "Qualified symbol name. REQUIRED. The highest-priority signal. If this uniquely identifies a symbol, other fields are ignored (but actual values are always returned in the response)."
            },
            "file_path": {
                "type": "string",
                "description": "Project-relative file path (e.g. 'src/foo.ts'). Supports suffix, basename, and fuzzy matching — no need to be exact. Used for ranking when qualified_name matches multiple symbols."
            },
            "line": {
                "type": "integer",
                "description": "1-based line number. Used for ranking within the same file. Off-by-small (1-2 lines) is tolerated; off-by-50+ becomes a weak signal."
            },
            "kind": {
                "type": "string",
                "description": "Symbol kind (function, method, class, ...). Weak tiebreaker only — cannot override file_path or line signals."
            },
            "language": {
                "type": "string",
                "description": "Language (typescript, rust, ...). Weakest signal, used only to break ties in multi-language repos."
            }
        }
    })
}

fn symbol_param_schema(string_desc: &str) -> serde_json::Value {
    json!({
        "oneOf": [
            {
                "type": "string",
                "description": string_desc
            },
            symbol_selector_schema()
        ]
    })
}


// ── Symbol tools ─────────────────────────────────────────────────────

fn make_symbol_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search".into(),
            description: "Search symbols by name within a project-relative scope. When a manual full structural index exists (built via CLI `atlas index`), scope is optional and defaults to the whole project. Small scopes are structurally parsed for precise function search; large scopes stay manifest-level and return a warning to narrow scope. Supports kind filter and background=true.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query": { "type": "string", "description": "Search query text" },
                    "scope": { "type": "string", "description": "Project-relative directory or file scope (e.g. 'drivers/net', 'src', 'kernel/sched'). Required for manifest-only indexes; optional when a full structural index exists. Use 'files' to discover indexed paths." },
                    "kind": { "type": "string", "description": "Optional SymbolKind filter (function, class, ...)" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                    "background": { "type": "boolean", "description": "Run search as background task (returns task_id for task_status polling)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["query".into()]),
            },
        },
        Tool {
            name: "symbol".into(),
            description: "Get symbol information by qualified name (symbol). view='detail' returns kind, location, signature, and caller/callee summaries (with optional source via includeCode). view='context' returns structured callers, callees, file peers, imports, dependencies, and precision tier. view='usages' returns reference usages. Default view is 'detail'.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. String matches are auto-resolved; use SymbolSelector object for precise disambiguation."),
                    "file_path": { "type": "string", "description": "File path relative to project root. When combined with 'line', resolves the symbol at this position (alternative to 'symbol' parameter)." },
                    "line": { "type": "integer", "description": "1-based line number. Used with 'file_path' for position-based symbol lookup." },
                    "column": { "type": "integer", "description": "1-based column number. Optional; defaults to 1 when omitted. Used with 'file_path' + 'line' for position-based symbol lookup." },
                    "view": {
                        "type": "string",
                        "enum": ["detail", "context", "usages"],
                        "description": "View mode: 'detail' for symbol info with optional source, 'context' for rich structured context, 'usages' for reference listing. Default: 'detail'."
                    },
                    "includeCode": { "type": "boolean", "description": "When true, includes the full source code of the enclosing definition (function/class/struct body). Default false (applies to view='detail' and 'context')." },
                    "includeFilePeers": { "type": "boolean", "description": "Include file peer symbols in context view (default: true). Set false for faster, smaller responses." },
                    "limit": { "type": "integer", "description": "Max results for view='usages' (default 50)." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// ── Graph tools ──────────────────────────────────────────────────────

fn make_graph_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "calls".into(),
            description: "Query the call graph around a symbol. direction='incoming' lists callers, 'outgoing' lists callees, 'both' returns bidirectional. depth>1 enables multi-hop traversal (replaces old callgraph). Use the edge_kinds parameter to query non-call edges for neighbor queries (default: [\"calls\",\"instantiates\",\"implements\"]; use [\"*\"] for all edge kinds).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches are auto-aggregated. Use SymbolSelector object for a precise single-symbol query."),
                    "direction": {
                        "type": "string",
                        "enum": ["incoming", "outgoing", "both"],
                        "description": "Edge direction: 'incoming' for callers, 'outgoing' for callees, 'both' for bidirectional (default 'both')."
                    },
                    "depth": { "type": "integer", "description": "Traversal depth (default 1, max 5). depth>1 enables multi-hop call-graph traversal." },
                    "limit": { "type": "integer", "description": "Max nodes returned (default depends on mode)." },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\",\"instantiates\",\"implements\"]. Use [\"*\"] or [] for all edge kinds (neighbor query mode)."
                    },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "explore".into(),
            description: "Symbol dossier: investigate a symbol's identity, source code, call evidence with callsite snippets, non-call relations (implements, extends, references, field access, etc.), file context (imports/exports/peers), and recommended next queries. For multi-hop graph traversal use atlas_calls.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches return candidates. Use SymbolSelector object for precise disambiguation."),
                    "source_mode": { "type": "string", "enum": ["excerpt", "full", "none"], "description": "Source display mode: excerpt (snippet around definition), full (entire symbol body, capped by max_source_bytes=65536), none (skip source). Default: excerpt." },
                    "source_lines": { "type": "integer", "description": "Max source lines to return when source_mode=excerpt. Default: 40." },
                    "evidence_limit": { "type": "integer", "description": "Max call evidence examples per direction. Default: 5." },
                    "relation_limit": { "type": "integer", "description": "Max non-call relation examples across all groups. Default: 20." },
                    "peer_limit": { "type": "integer", "description": "Max file peer symbols to return. Default: 12." },
                    "include_file_context": { "type": "boolean", "description": "Include imports, exports, and file peers. Default: true." },
                    "include_recommendations": { "type": "boolean", "description": "Include recommended next queries. Default: true." },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "path".into(),
            description: "Find the shortest path between two symbols through the graph (BFS). By default only follows call edges (calls, instantiates, implements, registers_callback). Use edge_kinds to override. Each edge hop includes direction (forward/reverse) and confidence. The path also includes breakpoints describing indirect hops, test code contamination, and reversed edges. Use prefer_production: true to prefer paths through production code over test files.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "from": symbol_param_schema("Source symbol qualified name. Ambiguous matches are auto-aggregated."),
                    "to": symbol_param_schema("Target symbol qualified name. Ambiguous matches are auto-aggregated."),
                    "max_depth": { "type": "integer", "description": "Max search depth (default 5, max 10)" },
                    "direction": {
                        "type": "string",
                        "enum": ["outgoing", "incoming", "both"],
                        "description": "Edge direction constraint during BFS: 'outgoing' (default) follows only forward/call edges, 'incoming' follows only reverse/caller edges, 'both' follows outgoing+incoming (use 'both' for reverse provenance / who-calls-X-to-reach-Y scenarios)."
                    },
                    "prefer_production": { "type": "boolean", "description": "When true, prefers paths through production (non-test) code. Test file nodes are deferred so production paths take priority even if longer by hop count. Default false." },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\", \"instantiates\", \"implements\", \"registers_callback\"]. Use [] or [\"*\"] for all edge kinds."
                    },
                    "includeCode": { "type": "boolean", "description": "When true, includes source code for each node in the path. Default false." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["from".into(), "to".into()]),
            },
        },
        Tool {
            name: "impact".into(),
            description: "Compute impact analysis: all symbols reachable from a given symbol (BFS bidirectionally — both downstream and upstream). Use semantic=true to include lifecycle invariants and branch diffs for impacted functions.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches are auto-aggregated."),
                    "depth": { "type": "integer", "description": "Max traversal depth (default 3, max 5)" },
                    "semantic": { "type": "boolean", "description": "When true, includes semantic impact analysis (lifecycle invariants, branch diffs) for impacted functions. Default false." },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// ── File graph tools ─────────────────────────────────────────────────

fn make_file_graph_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "file_dependencies".into(),
            description: "Find file-level dependencies by project-relative path. direction='outgoing' lists files that this file imports/includes, 'incoming' lists files that import/include this file, 'both' returns both directions. file_path is required (project-relative, no file_id).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_path": { "type": "string", "description": "Project-relative file path (e.g. 'src/main.rs'). Required." },
                    "direction": {
                        "type": "string",
                        "enum": ["incoming", "outgoing", "both"],
                        "description": "Direction: 'outgoing' (default) for imports by this file, 'incoming' for files importing this file, 'both' for both directions."
                    },
                    "limit": { "type": "integer", "description": "Max results (default 50)." },
                    "analysis": {
                        "type": "string",
                        "enum": ["manifest", "structural"],
                        "description": "Analysis mode: 'manifest' (default, fast — uses existing DB facts, no lazy extraction) vs 'structural' (bounded lazy refinement for better coverage).",
                        "default": "manifest"
                    },
                })),
                required: Some(vec!["file_path".into()]),
            },
        },
    ]
}

// ── Trace tools ──────────────────────────────────────────────────────

fn make_trace_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "trace".into(),
            description: "Source-level trace queries. kind='point' resolves a source position (file+line+column) to its full context. kind='variable' traces where a variable's value comes from (backward dataflow). kind='forward' traces the forward call chain from source to target. kind='callers' traces how a function gets invoked (backward call chain to farthest caller). Use file_id (hex) or file_path (project-relative) for position-based kinds; use symbol for kind='callers'; use from/to for kind='forward'.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "kind": {
                        "type": "string",
                        "description": "Trace operation kind.",
                        "oneOf": [
                            {
                                "const": "point",
                                "description": "Resolve a source position (file+line+column) to its full context — enclosing symbol, reference, scope, data node, and callsite. Requires structural index. Dataflow layer enables reference/callsite resolution; without it returns enclosing symbol only."
                            },
                            {
                                "const": "variable",
                                "description": "Trace where a variable's value comes from (backward intra-procedural dataflow). Requires dataflow layer for complete results; returns best-effort on structural-only projects."
                            },
                            {
                                "const": "forward",
                                "description": "Trace the forward call chain from source symbol to target symbol. Requires call-graph edges (available with structural index)."
                            },
                            {
                                "const": "callers",
                                "description": "Trace how a function gets invoked — backward call chain to the farthest caller. Requires call-graph edges (available with structural index)."
                            }
                        ]
                    },
                    "file_id": { "type": "string", "description": "File ID in hex (alternative to file_path for kind='point'/'variable')." },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts'). Alternative to file_id." },
                    "line": { "type": "integer", "description": "1-based line number (required for kind='point'/'variable')." },
                    "column": { "type": "integer", "description": "1-based column number (required for kind='point'/'variable')." },
                    "symbol": symbol_param_schema("Qualified symbol name. Use SymbolSelector object for precise disambiguation."),
                    "from": symbol_param_schema("Source qualified symbol name."),
                    "to": symbol_param_schema("Target qualified symbol name."),
                    "max_depth": { "type": "integer", "description": "Maximum traversal depth (kind='variable'/'forward'/'callers')." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: None,
            },
        },
    ]
}

// ── Semantic analysis tools ──────────────────────────────────────────

fn make_semantic_analysis_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "lifecycle".into(),
            description: "Analyze a field's lifecycle within a function using CFG effect annotations (C/C++). Walks the control-flow graph to track a field through allocate → use → free transitions, detecting use-after-free, double-free, and missing-free patterns. Triggers lazy structural extraction if CFG not yet built.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified function name to analyze (e.g. 'handle_request')" },
                    "field": { "type": "string", "description": "Field path to track (e.g. 'data->state.ptr' for C/C++ struct field access)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional C/C++ include roots" },
                })),
                required: Some(vec!["symbol".into(), "field".into()]),
            },
        },
        Tool {
            name: "branch_diff".into(),
            description: "Compare side effects of sibling branches (if/else, switch) within a function. Detects suspicious asymmetries — e.g., one branch frees a field but the other does not. Uses CFG effect annotations (C/C++ only initially).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified function name to analyze" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional C/C++ include roots" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// ── Domain rules tools (semantic analysis) ──────────────────────────

fn make_domain_rules_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "domain_rules".into(),
            description: "Manage domain rules for lifecycle analysis. action='add' defines which functions allocate/free/own memory (required: rule_kind [free_fn|alloc_fn|owned_pattern|cleanup_fn], pattern). action='list' shows rules, optionally filtered by source (builtin/learned/user). action='delete' removes a rule (required: rule_id). action='learn' auto-discovers rule candidates from project patterns (optional: min_confidence).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {
                        "type": "string",
                        "enum": ["add", "list", "delete", "learn"],
                        "description": "Action: 'add' to define a rule, 'list' to show rules, 'delete' to remove a rule, 'learn' to discover candidates."
                    },
                    "rule_kind": {
                        "type": "string",
                        "enum": ["free_fn", "alloc_fn", "owned_pattern", "cleanup_fn"],
                        "description": "Rule kind (required for action='add')."
                    },
                    "pattern": { "type": "string", "description": "Function name or field pattern (required for action='add')." },
                    "rule_id": { "type": "string", "description": "Rule ID (required for action='delete')." },
                    "source": { "type": "string", "enum": ["builtin", "learned", "user"], "description": "Filter by source (optional for action='list')." },
                    "confidence": { "type": "number", "description": "Confidence 0.0-1.0 (default 1.0 for user-declared)." },
                    "min_confidence": { "type": "number", "description": "Minimum confidence threshold for action='learn' (default 0.5)." },
                })),
                required: None,
            },
        },
    ]
}

// ── FP dispatch tools (C/C++) ───────────────────────────────────────

fn make_fp_dispatch_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "fp_dispatches".into(),
            description: "Manage function-pointer dispatch annotations for C/C++ code. action='add' declares a mapping from a struct's function-pointer field to its concrete target function (required: field_qname, target_qname). action='list' returns all declared annotations. action='delete' removes an annotation (required: annotation_id OR field_qname). After deletion, the materialized edge is removed on next re-index.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {
                        "type": "string",
                        "enum": ["add", "list", "delete"],
                        "description": "Action: 'add' to declare a dispatch, 'list' to show all annotations, 'delete' to remove one."
                    },
                    "field_qname": { "type": "string", "description": "Qualified name of the function-pointer field (required for action='add'; alternative identifier for action='delete')." },
                    "target_qname": { "type": "string", "description": "Qualified name of the target function (required for action='add')." },
                    "annotation_id": { "type": "string", "description": "Annotation ID from list (alternative identifier for action='delete')." },
                    "confidence": { "type": "number", "description": "Confidence score 0.0-1.0 (default 1.0 for user-declared)." },
                })),
                required: None,
            },
        },
    ]
}

// ── Task tools ───────────────────────────────────────────────────────

fn make_task_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "tasks".into(),
            description: "List all background tasks: active extraction jobs (from the store) and lazy extraction jobs. Optionally filter by query_id to see jobs for a specific query. Returns unified task view.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query_id": { "type": "string", "description": "Optional query_id to filter jobs." },
                })),
                required: None,
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
        Tool {
            name: "resume_task".into(),
            description: "Resume a previous query to get enhanced results after lazy background extraction completes. Returns the same format as the original tool with potentially richer data.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query_id": { "type": "string", "description": "The query_id from a previous tool call response" },
                })),
                required: Some(vec!["query_id".into()]),
            },
        },
    ]
}

pub fn make_all_tools() -> Vec<Tool> {
    let mut tools = Vec::new();
    tools.extend(make_project_tools());
    tools.extend(make_symbol_tools());
    tools.extend(make_graph_tools());
    tools.extend(make_file_graph_tools());
    tools.extend(make_trace_tools());
    tools.extend(make_semantic_analysis_tools());
    tools.extend(make_domain_rules_tools());
    tools.extend(make_fp_dispatch_tools());
    tools.extend(make_task_tools());
    tools
}

/// Merge edge-based file references into a dependents/dependencies JSON value.
fn merge_edge_deps(
    value: &mut serde_json::Value,
    edge_deps: &serde_json::Value,
    list_field: &str,
    total_field: &str,
) {
    if let Some(arr) = edge_deps.as_array() {
        if arr.is_empty() {
            return;
        }
        if let Some(deps) = value.get_mut(list_field) {
            if let Some(existing) = deps.as_array_mut() {
                for dep in arr {
                    existing.push(dep.clone());
                }
            }
        }
        if let Some(total) = value.get_mut(total_field) {
            if let Some(n) = total.as_u64() {
                *total = serde_json::json!(n + arr.len() as u64);
            }
        }
    }
}

// ===================================================================
// Facade handlers — dispatch merged tools to legacy handlers
// ===================================================================

impl ToolRouter {
    // ── project ──────────────────────────────────────────────────────

    /// Handle `project` tool — dispatch by `action`.
    pub(crate) fn handle_project(&mut self, args: &Value) -> (String, bool) {
        let action = get_str(args, "action");
        match action {
            "open" => self.handle_open_project(args),
            "status" => self.handle_status(),
            "files" => self.handle_files(args),
            "" => (
                "Missing required 'action' parameter. Must be one of: open, status, files"
                    .to_string(),
                true,
            ),
            other => (
                format!("Unknown action: '{other}'. Must be one of: open, status, files"),
                true,
            ),
        }
    }

    // ── symbol (facade) ──────────────────────────────────────────────

    /// Handle `symbol` tool — dispatch by `view` to legacy handlers.
    /// Remaps `symbol` → `qualified_name` (detail) or passes through as `symbol` (context/usages).
    pub(crate) fn handle_symbol(&mut self, ctx: &ToolCallContext, args: &Value) -> (String, bool) {
        // Position-based lookup: file_path + line as alternative to 'symbol'
        let file_path = get_str(args, "file_path");
        let line_opt = args.get("line").and_then(|v| v.as_u64()).map(|v| v as u32);
        if !file_path.is_empty() && line_opt.is_some() {
            return self.handle_symbol_by_position(ctx, file_path, line_opt.unwrap(), args);
        }

        let view = get_str(args, "view");
        // Parse symbol uniformly — handles string, object, and stringified-JSON
        let input = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = match &input {
            SymbolInput::Name(s) => s.clone(),
            SymbolInput::Selector(sel) => sel.qualified_name.clone(),
        };
        if qname.is_empty() {
            return ("Missing required 'symbol' parameter".to_string(), true);
        }

        match view {
            "detail" | "" => {
                // Pass original symbol value (string or structured selector) so
                // handle_symbol_detail can apply file_path/kind/line filtering.
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    args.get("symbol").cloned().unwrap_or(Value::String(qname.clone())),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_symbol_detail(&Value::Object(mapped))
            }
            "context" => {
                // Pass original symbol value — sub-handler parses via parse_symbol_input
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    args.get("symbol").cloned().unwrap_or(Value::String(qname.clone())),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("includeFilePeers") {
                    mapped.insert("includeFilePeers".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_context(ctx, &Value::Object(mapped))
            }
            "usages" => {
                // Pass original symbol value — sub-handler parses via parse_symbol_input
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    args.get("symbol").cloned().unwrap_or(Value::String(qname.clone())),
                );
                if let Some(v) = args.get("limit") {
                    mapped.insert("limit".into(), v.clone());
                }
                self.handle_usages(&Value::Object(mapped))
            }
            other => (
                format!("Unknown view: '{other}'. Must be one of: detail, context, usages"),
                true,
            ),
        }
    }
}

// ── calls dispatch ────────────────────────────────────────────────────

/// Result of [`resolve_calls_dispatch`] — which sub-handler should process the call.
pub(crate) enum CallsDispatch {
    CallGraph(serde_json::Value),
    Callers,
    Callees,
    Error(String),
}

// ── calls dispatch helper ─────────────────────────────────────────

pub(crate) fn resolve_calls_dispatch(
    args: &serde_json::Value,
) -> CallsDispatch {
    let direction = crate::tools::get_str(args, "direction");
    let depth = crate::tools::get_u64(args, "depth").unwrap_or(1);

    let raw_kinds = args.get("edge_kinds").and_then(|v| v.as_array());
    let (is_wildcard, edge_kinds): (bool, Vec<&str>) = match raw_kinds {
        Some(arr) => {
            let kinds: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            (kinds.is_empty(), kinds)
        }
        None => (false, vec!["calls", "instantiates", "implements"]),
    };
    let is_default_edges =
        !is_wildcard && edge_kinds == ["calls", "instantiates", "implements"];
    let is_custom_edges = !is_wildcard && !is_default_edges;

    if is_custom_edges
        || is_wildcard
        || depth > 1
        || direction == "both"
        || direction.is_empty()
    {
        let call_args = if args.get("depth").is_none() {
            let mut m = serde_json::Map::new();
            if let Some(obj) = args.as_object() {
                m.clone_from(obj);
            }
            m.insert(
                "depth".into(),
                serde_json::Value::Number(serde_json::Number::from(depth)),
            );
            serde_json::Value::Object(m)
        } else {
            args.clone()
        };
        CallsDispatch::CallGraph(call_args)
    } else {
        match direction {
            "incoming" => CallsDispatch::Callers,
            "outgoing" => CallsDispatch::Callees,
            other => CallsDispatch::Error(format!(
                "Unknown direction: '{other}'. Must be one of: incoming, outgoing, both"
            )),
        }
    }
}

impl ToolRouter {
    // ── calls ────────────────────────────────────────────────────────

    /// Handle `calls` tool — dispatch by `direction`/`depth`/`edge_kinds`.
    pub(crate) fn handle_calls(&mut self, args: &Value) -> (String, bool) {
        match resolve_calls_dispatch(args) {
            CallsDispatch::CallGraph(call_args) => self.handle_callgraph(&call_args),
            CallsDispatch::Callers => self.handle_callers(args),
            CallsDispatch::Callees => self.handle_callees(args),
            CallsDispatch::Error(e) => (e, true),
        }
    }

    // ── file_dependencies ────────────────────────────────────────────

    /// Handle `file_dependencies` tool — resolve file_path → file_id,
    /// dispatch by `direction`.
    pub(crate) fn handle_file_dependencies(&mut self, args: &Value) -> (String, bool) {
        let file_path = get_str(args, "file_path");
        if file_path.is_empty() {
            return ("Missing required 'file_path' parameter".to_string(), true);
        }
        let direction = get_str(args, "direction");
        if !matches!(direction, "incoming" | "outgoing" | "both" | "") {
            return (
                format!(
                    "Unknown direction: '{direction}'. Must be one of: incoming, outgoing, both"
                ),
                true,
            );
        }
        let analysis_mode = get_str(args, "analysis");
        let is_manifest = analysis_mode.is_empty() || analysis_mode == "manifest";
        if !is_manifest && analysis_mode != "structural" {
            return (
                format!(
                    "Unknown analysis mode: '{analysis_mode}'. Must be one of: manifest, structural"
                ),
                true,
            );
        }

        // Resolve file_path to file_id for legacy handlers
        let clean = file_path.trim_start_matches("./").trim_start_matches('/');
        let file_id = match self.store.resolve_file_id(&self.project_root, clean) {
            Ok(Some(id)) => id,
            Ok(None) => return (format!("File not found: {file_path}"), true),
            Err(e) => return (format!("Failed to resolve file: {e}"), true),
        };

        if is_manifest {
            return self.handle_file_dependencies_manifest(file_id, direction, args);
        }

        // ── structural mode ─────────────────────────────────────────────
        let mut lazy_warnings = Vec::new();
        let mut built_file_count = 0usize;
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut capability_mask = atlas_engine::structs::CapabilityMask::default();
        let mut coverage = "full";
        let mut reason: Option<&str> = None;

        if !self.cache.has_manual_full_index(&self.store) {
            let max_files = get_u64(args, "max_structural_files")
                .or_else(|| get_u64(args, "limit"))
                .unwrap_or(50) as usize;
            let file_ids = match direction {
                "incoming" | "both" => {
                    let (candidates, truncated) =
                        self.collect_edge_dependent_file_ids(&[file_id], max_files);
                    let mut result = vec![file_id];
                    result.extend(candidates);
                    if truncated {
                        coverage = "partial";
                        reason = Some("candidate_limit_exceeded");
                    }
                    result
                }
                _ => vec![file_id],
            };
            let outcome = self.ensure_structural_for_files(file_ids, vec![], None, None);
            lazy_warnings = outcome.warnings;
            built_file_count = outcome.built_file_ids.len();
            structural_tier = outcome.precision_tier;

            if let Some(ref lo) = outcome.lazy_outcome {
                capability_mask = lo.capability_mask;
                if lo.budget_exceeded {
                    coverage = "partial";
                    reason = Some("budget_exceeded");
                } else if lo.precision_tier
                    != atlas_engine::structs::precision::PrecisionTier::Exact
                {
                    coverage = "partial";
                }
            }

            if structural_tier == atlas_engine::structs::precision::PrecisionTier::Unavailable {
                reason = Some("no_structural_capability");
                coverage = "partial";
            }
        } else {
            capability_mask = self.store.derive_capability_for_files(&[file_id]);
        }

        let file_id_hex = file_id.to_hex();
        let mut mapped = serde_json::Map::new();
        mapped.insert("file_id".into(), Value::String(file_id_hex));
        if let Some(v) = args.get("limit") {
            mapped.insert("limit".into(), v.clone());
        }
        let mapped_args = Value::Object(mapped);

        match direction {
            "incoming" => {
                let (out, err) = self.handle_dependents(&mapped_args);
                (
                    add_dependency_analysis_contract(
                        out,
                        structural_tier,
                        built_file_count,
                        lazy_warnings,
                        coverage,
                        reason,
                        capability_mask,
                    ),
                    err,
                )
            }
            "outgoing" | "" => {
                let (out, err) = self.handle_dependencies(&mapped_args);
                (
                    add_dependency_analysis_contract(
                        out,
                        structural_tier,
                        built_file_count,
                        lazy_warnings,
                        coverage,
                        reason,
                        capability_mask,
                    ),
                    err,
                )
            }
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&mapped_args);
                let (in_str, in_err) = self.handle_dependents(&mapped_args);
                let result = json!({
                    "outgoing": serde_json::from_str::<Value>(&out_str).unwrap_or_default(),
                    "incoming": serde_json::from_str::<Value>(&in_str).unwrap_or_default(),
                    "analysis": {
                        "structural_precision_tier": structural_tier,
                        "lazy_built_files": built_file_count,
                        "warnings": lazy_warnings,
                    },
                    "analysis_contract": {
                        "coverage": coverage,
                        "reason": reason,
                        "precision_tier": structural_tier,
                        "capability_mask": capability_mask,
                    },
                });
                (
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                    out_err || in_err,
                )
            }
            _ => unreachable!("direction was validated above"),
        }
    }

    /// Manifest-mode file_dependencies — reads existing DB facts directly,
    /// no lazy structural extraction.
    fn handle_file_dependencies_manifest(
        &self,
        file_id: FileId,
        direction: &str,
        args: &Value,
    ) -> (String, bool) {
        let file_id_hex = file_id.to_hex();
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        match direction {
            "incoming" => {
                let (out_str, out_err) = self.handle_dependents(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let err = out_err;

                // Supplement with symbol_edges-based re-export / call dependencies
                let edge_deps = self.manifest_edge_dependents(
                    &file_id,
                    limit.saturating_sub(
                        serde_json::from_str::<Value>(&out_str)
                            .ok()
                            .and_then(|v| v["total_dependents"].as_u64())
                            .unwrap_or(0) as usize,
                    ),
                );
                let mut value =
                    serde_json::from_str::<Value>(&out_str).unwrap_or_else(|_| json!({}));
                merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
                let resp = add_analysis_contract_manifest(
                    serde_json::to_string_pretty(&value).unwrap_or_default(),
                );
                (resp, err)
            }
            "outgoing" | "" => {
                let (out_str, out_err) = self.handle_dependencies(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let err = out_err;

                // Supplement with symbol_edges-based export dependencies
                let edge_deps = self.manifest_edge_dependencies(
                    &file_id,
                    limit.saturating_sub(
                        serde_json::from_str::<Value>(&out_str)
                            .ok()
                            .and_then(|v| v["total_dependencies"].as_u64())
                            .unwrap_or(0) as usize,
                    ),
                );
                let mut value =
                    serde_json::from_str::<Value>(&out_str).unwrap_or_else(|_| json!({}));
                merge_edge_deps(&mut value, &edge_deps, "dependencies", "total_dependencies");
                let resp = add_analysis_contract_manifest(
                    serde_json::to_string_pretty(&value).unwrap_or_default(),
                );
                (resp, err)
            }
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let (in_str, in_err) = self.handle_dependents(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let err = out_err || in_err;

                let edge_out = self.manifest_edge_dependencies(&file_id, limit);
                let edge_in = self.manifest_edge_dependents(&file_id, limit);

                let mut outgoing = serde_json::from_str::<Value>(&out_str).unwrap_or_default();
                let mut incoming = serde_json::from_str::<Value>(&in_str).unwrap_or_default();
                merge_edge_deps(&mut outgoing, &edge_out, "dependencies", "total_dependencies");
                merge_edge_deps(&mut incoming, &edge_in, "dependents", "total_dependents");

                let result = json!({
                    "outgoing": outgoing,
                    "incoming": incoming,
                    "analysis_contract": {
                        "coverage": "full",
                        "reason": Value::Null,
                    },
                });
                (
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                    err,
                )
            }
            _ => unreachable!("direction was validated above"),
        }
    }

    /// Collect candidate file IDs for incoming structural file_dependencies.
    ///
    /// Uses manifest symbol edges to discover files whose symbols have edges
    /// targeting symbols in the given `target_file_ids`.  This is the bounded
    /// candidate-discovery counterpart of `manifest_edge_dependents` (which
    /// returns JSON rows); here we only collect the unique `FileId`s and cap at
    /// `max_files`.
    ///
    /// Returns `(file_ids, truncated)` where `truncated` is `true` when there
    /// were more unique source-file candidates than `max_files`.
    fn collect_edge_dependent_file_ids(
        &self,
        target_file_ids: &[FileId],
        max_files: usize,
    ) -> (Vec<FileId>, bool) {
        if max_files == 0 || target_file_ids.is_empty() {
            return (Vec::new(), false);
        }

        // Gather all symbols in the target file(s).
        let mut our_ids: HashSet<SymbolId> = HashSet::new();
        for fid in target_file_ids {
            let syms = match self.store.find_symbols_by_file(fid) {
                Ok(s) => s,
                Err(_) => continue,
            };
            for sym in &syms {
                our_ids.insert(sym.id);
            }
        }
        if our_ids.is_empty() {
            return (Vec::new(), false);
        }

        // Edges whose target is in `our_ids` and whose source is NOT → that
        // source's file is a candidate dependent.
        let edges = match self.store.find_edges_for_files(target_file_ids) {
            Ok(e) => e,
            Err(_) => return (Vec::new(), false),
        };

        let mut source_ids: HashSet<SymbolId> = HashSet::new();
        for edge in &edges {
            if our_ids.contains(&edge.target) && !our_ids.contains(&edge.source) {
                source_ids.insert(edge.source);
            }
        }

        if source_ids.is_empty() {
            return (Vec::new(), false);
        }

        let ids_vec: Vec<SymbolId> = source_ids.into_iter().collect();
        let symbols = match self.store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return (Vec::new(), false),
        };

        let mut file_ids: HashSet<FileId> = HashSet::new();
        let mut truncated = false;
        for sym in &symbols {
            if file_ids.len() >= max_files {
                truncated = true;
                break;
            }
            file_ids.insert(sym.file_id);
        }

        (file_ids.into_iter().collect(), truncated)
    }

    /// Query symbol_edges for incoming file dependencies (manifest mode).
    /// Returns files whose symbols have edges targeting symbols in `file_id`.
    fn manifest_edge_dependents(&self, file_id: &FileId, max_results: usize) -> Value {
        if max_results == 0 {
            return json!([]);
        }
        let our_symbols = match self.store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.store.find_edges_for_files(&[*file_id]) {
            Ok(e) => e,
            Err(_) => return json!([]),
        };

        // Incoming: edges where target is in our file → source's file depends on us
        let mut source_ids: HashSet<SymbolId> = HashSet::new();
        for edge in &edges {
            if our_set.contains(&edge.target) && !our_set.contains(&edge.source) {
                source_ids.insert(edge.source);
            }
        }

        if source_ids.is_empty() {
            return json!([]);
        }
        let ids_vec: Vec<SymbolId> = source_ids.into_iter().collect();
        let symbols = match self.store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        let mut file_paths: HashSet<String> = HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for sym in &symbols {
            if file_paths.len() >= max_results {
                break;
            }
            let path = self.resolve_file_path(&sym.file_id);
            if file_paths.insert(path.clone()) {
                results.push(json!({
                    "file": path,
                    "import": "symbol_edge",
                }));
            }
        }
        json!(results)
    }

    /// Query symbol_edges for outgoing file dependencies (manifest mode).
    /// Returns files whose symbols are targeted by symbols in `file_id`.
    fn manifest_edge_dependencies(&self, file_id: &FileId, max_results: usize) -> Value {
        if max_results == 0 {
            return json!([]);
        }
        let our_symbols = match self.store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.store.find_edges_for_files(&[*file_id]) {
            Ok(e) => e,
            Err(_) => return json!([]),
        };

        // Outgoing: edges where source is in our file → target's file is our dependency
        let mut target_ids: HashSet<SymbolId> = HashSet::new();
        for edge in &edges {
            if our_set.contains(&edge.source) && !our_set.contains(&edge.target) {
                target_ids.insert(edge.target);
            }
        }

        if target_ids.is_empty() {
            return json!([]);
        }
        let ids_vec: Vec<SymbolId> = target_ids.into_iter().collect();
        let symbols = match self.store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        let mut file_paths: HashSet<String> = HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for sym in &symbols {
            if file_paths.len() >= max_results {
                break;
            }
            let path = self.resolve_file_path(&sym.file_id);
            if file_paths.insert(path.clone()) {
                results.push(json!({
                    "module": path,
                    "imported_name": sym.name,
                    "kind": "symbol_edge",
                }));
            }
        }
        json!(results)
    }

    // ── trace ────────────────────────────────────────────────────────

    /// Handle `trace` tool — dispatch by `kind`.
    pub(crate) fn handle_trace(&mut self, ctx: &ToolCallContext, args: &Value) -> (String, bool) {
        let kind = get_str(args, "kind");
        match kind {
            "point" => self.handle_trace_point(ctx, args),
            "variable" => self.handle_trace_variable(args),
            "forward" => self.handle_trace_forward(args),
            "callers" => self.handle_trace_caller_path(args),
            "" => (
                "Missing required 'kind' parameter. Must be one of: point, variable, forward, callers"
                    .to_string(),
                true,
            ),
            other => (
                format!(
                    "Unknown kind: '{other}'. Must be one of: point, variable, forward, callers"
                ),
                true,
            ),
        }
    }

    // ── fp_dispatches ────────────────────────────────────────────────

    /// Handle `fp_dispatches` tool — dispatch by `action`.
    pub(crate) fn handle_fp_dispatches(&mut self, args: &Value) -> (String, bool) {
        let action = get_str(args, "action");
        match action {
            "add" => self.handle_annotate_fp_dispatch(args),
            "list" | "" => self.handle_list_fp_annotations(),
            "delete" => self.handle_delete_fp_annotation(args),
            other => (
                format!("Unknown action: '{other}'. Must be one of: add, list, delete"),
                true,
            ),
        }
    }

    // ── domain_rules ─────────────────────────────────────────────────

    /// Handle `domain_rules` tool — dispatch by `action`.
    pub(crate) fn handle_domain_rules(&mut self, args: &Value) -> (String, bool) {
        let action = get_str(args, "action");
        match action {
            "add" => self.handle_atlas_annotate(args),
            "list" | "" => self.handle_atlas_domain_rules(args),
            "delete" => self.handle_atlas_domain_rules(args),
            "learn" => self.handle_atlas_rule_learn(args),
            other => (
                format!("Unknown action: '{other}'. Must be one of: add, list, delete, learn"),
                true,
            ),
        }
    }

    // ── tasks ────────────────────────────────────────────────────────

    /// Handle `tasks` tool — aggregate active jobs + atlas jobs.
    pub(crate) fn handle_tasks(&mut self, args: &Value) -> (String, bool) {
        let query_id = get_str_opt(args, "query_id");

        let (jobs_str, jobs_err) = self.handle_jobs();
        let atlas_args = if let Some(qid) = query_id {
            let mut m = serde_json::Map::new();
            m.insert("query_id".into(), Value::String(qid.to_string()));
            Value::Object(m)
        } else {
            Value::Object(serde_json::Map::new())
        };
        let (atlas_str, atlas_err) = self.handle_atlas_jobs(&atlas_args);

        let result = json!({
            "active_extraction_jobs": serde_json::from_str::<Value>(&jobs_str).unwrap_or_default(),
            "atlas_jobs": serde_json::from_str::<Value>(&atlas_str).unwrap_or_default(),
        });
        (
            serde_json::to_string_pretty(&result).unwrap_or_default(),
            jobs_err || atlas_err,
        )
    }

    /// Synchronous direct-call wrapper for `wait_for_task`.
    ///
    /// The MCP service layer uses the async implementation so it does not block
    /// the runtime. This path exists for tests and embedded callers that invoke
    /// `ToolRouter::call_tool` directly.
    pub(crate) fn handle_wait_for_task_sync(&mut self, args: &Value) -> (String, bool) {
        let wfr = wait_for::handle_wait_for_task_sync(&self.async_state.task_manager, args);
        if !wfr.task_is_project_completed {
            return (wfr.json_text, wfr.is_error);
        }

        let task_id = get_str(args, "task_id");
        let mut val: Value = serde_json::from_str(&wfr.json_text).unwrap_or_default();
        if let Some(project) = self.activate_pending_project_for_task(task_id) {
            val["activation"] = Value::String("activated".into());
            val["activated_project"] = Value::String(project);
        } else {
            val["activation"] = Value::String("already_activated".into());
        }
        (
            serde_json::to_string_pretty(&val).unwrap_or_else(|e| e.to_string()),
            wfr.is_error,
        )
    }
}

// -------------------------------------------------------------------
// Shared arg-parsing helpers
// -------------------------------------------------------------------

fn add_dependency_analysis_contract(
    response: String,
    structural_tier: atlas_engine::structs::precision::PrecisionTier,
    built_file_count: usize,
    warnings: Vec<String>,
    coverage: &str,
    reason: Option<&str>,
    capability_mask: atlas_engine::structs::CapabilityMask,
) -> String {
    let mut value = serde_json::from_str::<Value>(&response).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "analysis".into(),
            json!({
                "structural_precision_tier": structural_tier,
                "lazy_built_files": built_file_count,
                "warnings": warnings,
            }),
        );
        obj.insert(
            "analysis_contract".into(),
            json!({
                "coverage": coverage,
                "reason": reason,
                "precision_tier": structural_tier,
                "capability_mask": capability_mask,
            }),
        );
    }
    serde_json::to_string_pretty(&value).unwrap_or(response)
}

/// Add a minimal analysis_contract for manifest-mode responses.
fn add_analysis_contract_manifest(response: String) -> String {
    let mut value = serde_json::from_str::<Value>(&response).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "analysis_contract".into(),
            json!({
                "coverage": "full",
                "reason": Value::Null,
            }),
        );
    }
    serde_json::to_string_pretty(&value).unwrap_or(response)
}

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

/// Normalize a project-relative path: replace backslashes, remove leading ./
/// components, collapse redundant slashes, reject path-escape patterns.
fn normalize_project_relative_path(raw: &str) -> Option<String> {
    let raw = raw.replace('\\', "/");
    let raw = raw.trim_start_matches("./");
    let raw = raw.trim_end_matches('/');

    let mut components = Vec::new();
    for comp in raw.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                if components.is_empty() {
                    return None; // escape attempt
                }
                components.pop();
            }
            _ => components.push(comp),
        }
    }

    if components.is_empty() {
        return None;
    }
    Some(components.join("/"))
}

// ── Position-based symbol lookup ──────────────────────────────────────

/// Check if a [`SymbolKind`] represents a definition entity (not a reference or unknown).
///
/// All currently defined [`SymbolKind`] variants are definitions, so this
/// always returns `true`.  Kept as a named predicate for clarity and
/// future-proofing in case reference/import kinds are added later.
fn is_definition_kind(_kind: &atlas_engine::SymbolKind) -> bool {
    // All current SymbolKind values (File, Module, Class, Struct,
    // Interface, Trait, Enum, EnumMember, Function, Method, Property,
    // Field, Variable, Constant, TypeAlias, Namespace, Parameter,
    // Constructor, Macro, Decorator, Package) are definitions.
    true
}

impl ToolRouter {
    /// Handle symbol lookup by file position (`file_path` + `line` + optional `column`).
    ///
    /// Resolves the position to the nearest enclosing symbol definition, then
    /// delegates to [`handle_symbol_detail`] with the found `qualified_name`.
    fn handle_symbol_by_position(
        &mut self,
        ctx: &ToolCallContext,
        file_path: &str,
        line: u32,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let column = args
            .get("column")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(1);

        // Normalize and resolve file_path to FileId
        let normalized = match normalize_project_relative_path(file_path) {
            Some(p) => p,
            None => {
                return (
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": false,
                        "error": format!(
                            "Invalid file path: '{}'. Path must be project-relative and must not escape the project root.",
                            file_path
                        ),
                    }))
                    .unwrap_or_default(),
                    true,
                );
            }
        };
        let file_id = FileId::generate(&normalized);
        if self.store.get_file(&file_id).ok().flatten().is_none() {
            return (
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": false,
                    "error": format!(
                        "File not found in project: '{}'. Use the 'files' action on the 'project' tool to list indexed files.",
                        file_path
                    ),
                }))
                .unwrap_or_default(),
                true,
            );
        }

        // Ensure structural layer is available for this file
        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        let investigation = self.investigation_state.active_investigation.clone();
        let query_id = Self::generate_query_id();
        let outcome = self.ensure_structural_for_files(
            std::collections::HashSet::from([file_id]),
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        let mut warnings: Vec<String> = root_warnings;
        warnings.extend(outcome.warnings);

        // Find all symbols in the file
        let symbols = match self.store.find_symbols_by_file(&file_id) {
            Ok(syms) => syms,
            Err(e) => {
                let mut err = serde_json::json!({
                    "ok": false,
                    "error": format!("Failed to read symbols for '{}': {}", file_path, e),
                });
                add_json_warnings(&mut err, warnings, vec![]);
                return (serde_json::to_string_pretty(&err).unwrap_or_default(), true);
            }
        };

        // Filter: symbol range must contain the position AND be a definition kind.
        // TextRange uses 0-based lines/columns; user input is 1-based.
        let target_line = line.saturating_sub(1);
        let target_col = column;
        let mut candidates: Vec<&atlas_engine::SymbolDef> = symbols
            .iter()
            .filter(|s| {
                is_definition_kind(&s.kind)
                    && s.range.start_line <= target_line
                    && target_line <= s.range.end_line
                    && s.range.start_column <= target_col
                    && target_col <= s.range.end_column
            })
            .collect();

        if candidates.is_empty() {
            let mut err = serde_json::json!({
                "ok": false,
                "error": format!(
                    "No symbol definition found at {}:{} (column {})",
                    file_path, line, column
                ),
            });
            add_json_warnings(&mut err, warnings, vec![]);
            return (serde_json::to_string_pretty(&err).unwrap_or_default(), true);
        }

        // Pick innermost (smallest range): sort by (line_span, column_span)
        candidates.sort_by_key(|s| {
            (s.range.end_line - s.range.start_line) * 1_000_000
                + (s.range.end_column - s.range.start_column)
        });
        let symbol = candidates[0];

        // Dispatch to the appropriate sub-handler based on view.
        let view = args
            .get("view")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match view {
            "detail" | "" => {
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "qualified_name".into(),
                    serde_json::Value::String(symbol.qualified_name.clone()),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }

                let (mut result, is_error) =
                    self.handle_symbol_detail(&serde_json::Value::Object(mapped));
                if !warnings.is_empty() && !is_error {
                    if let Ok(mut parsed) =
                        serde_json::from_str::<serde_json::Value>(&result)
                    {
                        add_json_warnings(&mut parsed, warnings.clone(), vec![]);
                        if let Ok(pretty) = serde_json::to_string_pretty(&parsed) {
                            result = pretty;
                        }
                    }
                }
                (result, is_error)
            }
            "context" => {
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    serde_json::Value::String(symbol.qualified_name.clone()),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("includeFilePeers") {
                    mapped.insert("includeFilePeers".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_context(ctx, &serde_json::Value::Object(mapped))
            }
            "usages" => {
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    serde_json::Value::String(symbol.qualified_name.clone()),
                );
                if let Some(v) = args.get("limit") {
                    mapped.insert("limit".into(), v.clone());
                }
                self.handle_usages(&serde_json::Value::Object(mapped))
            }
            other => (
                format!(
                    "Unknown view: '{other}'. Must be one of: detail, context, usages"
                ),
                true,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;

    // ── Helper: create an in-memory store with schema ────────────────────
    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    // ── Helper: register a minimal TypeScript file ──────────────────────
    fn register_test_file(store: &Store, path: &str) -> FileId {
        let file_id = FileId::generate(path);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id,
                path: path.into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        file_id
    }

    // ── Helper: insert a minimal function symbol ────────────────────────
    fn insert_test_symbol(store: &Store, file_id: FileId, name: &str) {
        insert_test_symbol_with_signature(store, file_id, name, None);
    }

    fn insert_test_symbol_with_signature(
        store: &Store,
        file_id: FileId,
        name: &str,
        signature: Option<&str>,
    ) {
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let sym = atlas_engine::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", name, "function", None),
            kind: atlas_engine::SymbolKind::Function,
            name: name.into(),
            qualified_name: format!("{name}.{name}"),
            symbol_path: vec![name.into()],
            file_id,
            language: atlas_engine::Language::TypeScript,
            range,
            name_range: range,
            signature: signature.map(str::to_string),
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
    }

    #[test]
    fn normalize_project_relative_path_accepts_include() {
        assert_eq!(
            normalize_project_relative_path("include"),
            Some("include".into())
        );
    }

    #[test]
    fn normalize_project_relative_path_strips_dot_slash() {
        assert_eq!(
            normalize_project_relative_path("./src/include"),
            Some("src/include".into())
        );
    }

    #[test]
    fn normalize_project_relative_path_converts_backslash() {
        assert_eq!(
            normalize_project_relative_path("src\\include"),
            Some("src/include".into())
        );
    }

    #[test]
    fn normalize_project_relative_path_rejects_escape() {
        assert_eq!(normalize_project_relative_path("../outside"), None);
    }

    #[test]
    fn normalize_project_relative_path_collapses_double_dot_within() {
        assert_eq!(
            normalize_project_relative_path("a/b/../c"),
            Some("a/c".into())
        );
    }

    #[test]
    fn structural_ensure_outcome_default() {
        let outcome = StructuralEnsureOutcome {
            warnings: vec![],
            built_file_ids: vec![],
            precision_tier: atlas_engine::structs::precision::PrecisionTier::Unavailable,
            lazy_outcome: None,
        };
        assert!(outcome.warnings.is_empty());
        assert!(outcome.built_file_ids.is_empty());
    }

    #[test]
    fn warnings_to_trace_diagnostics_converts() {
        let diags = warnings_to_trace_diagnostics(vec!["test error".into()], "test_code");
        assert!(!diags.is_empty());
        assert_eq!(diags[0].code, Some("test_code".into()));
        assert_eq!(diags[0].message, "test error");
    }

    #[test]
    fn add_json_warnings_empty_is_noop() {
        let mut val = serde_json::json!({});
        add_json_warnings(&mut val, vec![], vec![]);
        assert!(val.get("warnings").is_none());
    }

    #[test]
    fn add_json_warnings_merges() {
        let mut val = serde_json::json!({});
        add_json_warnings(&mut val, vec!["r1".into()], vec!["l1".into()]);
        let warns = val["warnings"].as_array().unwrap();
        assert_eq!(warns.len(), 2);
    }

    #[test]
    fn status_reports_manifest_mode_from_fresh_layers() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        store
            .upsert_file_extraction_state(
                &file_id,
                "manifest",
                "hash1",
                "complete",
                atlas_engine::structs::CapabilityMask::default(),
            )
            .unwrap();

        let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let (resp_str, is_error) = router.handle_status();
        assert!(!is_error, "status failed: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["index"]["mode"].as_str(), Some("manifest"));
        assert_eq!(resp["index"]["active_extraction_jobs"].as_u64(), Some(0));
    }

    #[test]
    fn jobs_lists_active_extraction_jobs() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        store
            .claim_file_extraction_job(&file_id, "structural", Some("test"), None, Some(30_000))
            .unwrap();

        let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let (resp_str, is_error) = router.handle_jobs();
        assert!(!is_error, "jobs failed: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let jobs = resp["active_jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["layer"].as_str(), Some("structural"));
    }

    // ── Regression: include_roots validation produces diagnostics ────

    #[test]
    fn trace_point_invalid_include_roots_returns_diagnostics() {
        let store = test_store();
        register_test_file(&store, "test.ts");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

        let args = serde_json::json!({
            "file_path": "test.ts",
            "line": 1,
            "column": 1,
            "include_roots": ["/absolute/rejected"]
        });

        let (resp_str, _is_error) = router.handle_trace_point(&ToolCallContext::empty(), &args);
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let diags = resp["diagnostics"].as_array().unwrap();
        assert!(
            !diags.is_empty(),
            "Expected diagnostics for invalid include_roots"
        );
        let codes: Vec<&str> = diags.iter().filter_map(|d| d["code"].as_str()).collect();
        assert!(
            codes.contains(&"include_roots_warning"),
            "Expected include_roots_warning code, got: {codes:?}"
        );
    }

    #[test]
    fn trace_variable_invalid_include_roots_returns_diagnostics() {
        let store = test_store();
        register_test_file(&store, "test.ts");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

        let args = serde_json::json!({
            "file_path": "test.ts",
            "line": 1,
            "column": 1,
            "max_depth": 5,
            "include_roots": ["/absolute/rejected"]
        });

        let (resp_str, _) = router.handle_trace_variable(&args);
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let diags = resp["diagnostics"].as_array();
        assert!(diags.is_some(), "Expected diagnostics");
        let codes: Vec<&str> = diags
            .unwrap()
            .iter()
            .filter_map(|d| d["code"].as_str())
            .collect();
        assert!(
            codes.contains(&"include_roots_warning"),
            "Expected include_roots_warning, got: {codes:?}"
        );
    }

    // ── Regression: symbol/context with invalid include_roots → warnings ──

    #[test]
    fn symbol_existing_invalid_include_roots_returns_warning() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol(&store, file_id, "test_func");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "symbol": "test_func.test_func",
            "include_roots": ["/absolute/rejected"]
        });

        let (resp_str, is_error) = router.handle_symbol(&ToolCallContext::empty(), &args);
        assert!(!is_error, "Expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let warns = resp["warnings"].as_array();
        assert!(warns.is_some(), "Expected 'warnings' field in: {resp_str}");
        assert!(
            !warns.unwrap().is_empty(),
            "Expected non-empty warnings in: {resp_str}"
        );
    }

    #[test]
    fn symbol_detail_returns_stored_signature() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol_with_signature(
            &store,
            file_id,
            "test_func",
            Some("(arg: string): void"),
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "symbol": "test_func.test_func",
            "view": "detail"
        });

        let (resp_str, is_error) = router.handle_symbol(&ToolCallContext::empty(), &args);
        assert!(!is_error, "Expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(
            resp["signature"].as_str(),
            Some("(arg: string): void"),
            "symbol detail must pass through the stored SymbolDef.signature"
        );
    }

    #[test]
    fn context_existing_invalid_include_roots_returns_warning() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol(&store, file_id, "test_func");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "symbol": "test_func.test_func",
            "include_roots": ["/absolute/rejected"]
        });

        let (resp_str, is_error) = router.handle_context(&ToolCallContext::empty(), &args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let warns = resp["warnings"].as_array();
        assert!(warns.is_some(), "Expected 'warnings' field in: {resp_str}");
        assert!(
            !warns.unwrap().is_empty(),
            "Expected non-empty warnings in: {resp_str}"
        );
    }

    #[test]
    fn context_include_file_peers_false_produces_empty_file_peers() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol(&store, file_id, "test_func");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "symbol": "test_func.test_func",
            "includeFilePeers": false,
        });

        let (resp_str, is_error) = router.handle_context(&ToolCallContext::empty(), &args);
        assert!(!is_error, "Expected success, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let file_peers = resp["file_peers"].as_array();
        assert!(
            file_peers.is_some(),
            "Expected 'file_peers' field in: {resp_str}"
        );
        assert!(
            file_peers.unwrap().is_empty(),
            "Expected empty file_peers when includeFilePeers=false, got: {resp_str}"
        );
    }

    // ── file_dependencies tests ──────────────────────────────────────────

    /// Helper: insert a symbol edge between two symbols.
    fn insert_test_edge(store: &Store, source: SymbolId, target: SymbolId) {
        use atlas_engine::Confidence;
        use atlas_engine::EdgeKind;
        use atlas_engine::Provenance;
        let edge = atlas_engine::RawEdge::new(
            atlas_engine::EdgeId::generate(&source, &target, "calls", None, "tree_sitter"),
            source,
            target,
            EdgeKind::Calls,
            Confidence::new(1.0),
            Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();
    }

    /// Helper: insert an import from one file to another.
    fn insert_test_import(store: &Store, from_file: FileId, to_path: &str, imported_name: &str) {
        use atlas_engine::ImportKind;
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let import = atlas_engine::ImportDef {
            id: atlas_engine::ImportId::generate(
                &from_file,
                "import",
                to_path,
                Some(imported_name),
                0,
            ),
            file_id: from_file,
            kind: ImportKind::Import,
            module: to_path.to_string(),
            imported_name: imported_name.to_string(),
            local_name: None,
            is_wildcard: false,
            is_relative: false,
            range,
            alias: None,
        };
        store.insert_imports(&[import]).unwrap();
    }

    #[test]
    fn manifest_incoming_returns_correct_deps() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        // File B imports from A, and has a call edge to A's symbol
        let sym_a = SymbolId::generate(&file_a, "typescript", "foo", "function", None);
        let sym_b = SymbolId::generate(&file_b, "typescript", "bar", "function", None);
        insert_test_symbol(&store, file_a, "foo");
        insert_test_symbol(&store, file_b, "bar");

        // Edge: B's bar calls A's foo → B depends on A
        insert_test_edge(&store, sym_b, sym_a);

        // Import: B imports from a.ts
        insert_test_import(&store, file_b, "a.ts", "foo");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "incoming",
            "analysis": "manifest",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        // Should have the analysis_contract with coverage=full
        let contract = &resp["analysis_contract"];
        assert_eq!(contract["coverage"].as_str(), Some("full"));
        assert!(contract["reason"].is_null());

        // Should have at least the import-based dependent (b.ts)
        let deps = resp["dependents"].as_array().unwrap();
        let dep_files: Vec<&str> = deps.iter().filter_map(|d| d["file"].as_str()).collect();
        assert!(
            dep_files.contains(&"b.ts"),
            "Expected b.ts in dependents, got: {dep_files:?}"
        );

        // The edge-based dependent should also be there (from symbol_edges)
        // Both import and edge point to b.ts, deduplication should result in one entry
        assert!(
            dep_files.len() >= 1,
            "Expected at least one dependent, got: {resp_str}"
        );
    }

    #[test]
    fn manifest_outgoing_returns_correct_deps() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        let sym_a = SymbolId::generate(&file_a, "typescript", "foo", "function", None);
        let sym_b = SymbolId::generate(&file_b, "typescript", "bar", "function", None);
        insert_test_symbol(&store, file_a, "foo");
        insert_test_symbol(&store, file_b, "bar");

        // Edge: A's foo calls B's bar → A depends on B
        insert_test_edge(&store, sym_a, sym_b);

        // Import: A imports from b.ts
        insert_test_import(&store, file_a, "b.ts", "bar");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "outgoing",
            "analysis": "manifest",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let contract = &resp["analysis_contract"];
        assert_eq!(contract["coverage"].as_str(), Some("full"));
        assert!(contract["reason"].is_null());

        let deps = resp["dependencies"].as_array().unwrap();
        let dep_modules: Vec<&str> = deps.iter().filter_map(|d| d["module"].as_str()).collect();
        assert!(
            dep_modules.contains(&"b.ts"),
            "Expected b.ts in dependencies, got: {dep_modules:?}"
        );
    }

    #[test]
    fn manifest_both_returns_analysis_contract() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        let sym_a = SymbolId::generate(&file_a, "typescript", "foo", "function", None);
        let sym_b = SymbolId::generate(&file_b, "typescript", "bar", "function", None);
        insert_test_symbol(&store, file_a, "foo");
        insert_test_symbol(&store, file_b, "bar");

        insert_test_edge(&store, sym_b, sym_a); // B → A: incoming
        insert_test_edge(&store, sym_a, sym_b); // A → B: outgoing
        insert_test_import(&store, file_b, "a.ts", "foo");
        insert_test_import(&store, file_a, "b.ts", "bar");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "both",
            "analysis": "manifest",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let contract = &resp["analysis_contract"];
        assert_eq!(contract["coverage"].as_str(), Some("full"));
        assert!(contract["reason"].is_null());
    }

    #[test]
    fn structural_returns_analysis_contract() {
        let store = test_store();
        let _file_a = register_test_file(&store, "a.ts");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "incoming",
            "analysis": "structural",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        // analysis_contract must be present
        let contract = &resp["analysis_contract"];
        assert!(
            contract.get("coverage").is_some(),
            "analysis_contract missing coverage field: {resp_str}"
        );
        // structural mode should have precision_tier and capability_mask
        assert!(
            contract.get("precision_tier").is_some(),
            "analysis_contract missing precision_tier: {resp_str}"
        );
        assert!(
            contract.get("capability_mask").is_some(),
            "analysis_contract missing capability_mask: {resp_str}"
        );
    }

    #[test]
    fn analysis_contract_manifest_full_coverage() {
        let store = test_store();
        let _file_a = register_test_file(&store, "a.ts");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "incoming",
            "analysis": "manifest",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let contract = &resp["analysis_contract"];
        assert_eq!(
            contract["coverage"].as_str(),
            Some("full"),
            "manifest mode must have full coverage: {resp_str}"
        );
        assert!(
            contract["reason"].is_null(),
            "manifest mode reason must be null: {resp_str}"
        );
    }

    #[test]
    fn manifest_default_when_analysis_omitted() {
        let store = test_store();
        let _file_a = register_test_file(&store, "a.ts");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        // Omit analysis parameter — should default to manifest
        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "incoming",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let contract = &resp["analysis_contract"];
        assert_eq!(
            contract["coverage"].as_str(),
            Some("full"),
            "default mode (manifest) must have full coverage: {resp_str}"
        );
    }

    #[test]
    fn unknown_analysis_mode_returns_error() {
        let store = test_store();
        let _file_a = register_test_file(&store, "a.ts");

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "incoming",
            "analysis": "invalid",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(
            is_error,
            "Expected error for unknown analysis mode, got: {resp_str}"
        );
        assert!(
            resp_str.contains("Unknown analysis mode"),
            "Expected error message, got: {resp_str}"
        );
    }

    #[test]
    fn manifest_edge_dependencies_via_calls() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");
        let file_c = register_test_file(&store, "c.ts");

        let sym_a = SymbolId::generate(&file_a, "typescript", "target", "function", None);
        let sym_b = SymbolId::generate(&file_b, "typescript", "caller_b", "function", None);
        let sym_c = SymbolId::generate(&file_c, "typescript", "caller_c", "function", None);
        insert_test_symbol(&store, file_a, "target");
        insert_test_symbol(&store, file_b, "caller_b");
        insert_test_symbol(&store, file_c, "caller_c");

        // Both B and C call A
        insert_test_edge(&store, sym_b, sym_a);
        insert_test_edge(&store, sym_c, sym_a);

        // No imports — edge-based deps only
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "file_path": "a.ts",
            "direction": "incoming",
            "analysis": "manifest",
        });
        let (resp_str, is_error) = router.handle_file_dependencies(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let deps = resp["dependents"].as_array().unwrap();
        let dep_files: Vec<&str> = deps.iter().filter_map(|d| d["file"].as_str()).collect();
        assert!(
            dep_files.contains(&"b.ts"),
            "Expected edge-based dependent b.ts, got: {dep_files:?}"
        );
        assert!(
            dep_files.contains(&"c.ts"),
            "Expected edge-based dependent c.ts, got: {dep_files:?}"
        );
    }

    // ── ToolCallContext tests ────────────────────────────────────────────

    #[test]
    fn tool_call_context_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ToolCallContext>();
        assert_sync::<ToolCallContext>();
    }

    #[test]
    fn tool_call_context_empty_does_not_panic_on_send_progress() {
        let ctx = ToolCallContext::empty();
        // Should be a no-op — no panic when no progress_sender is set.
        ctx.send_progress(0.5, "test message");
        ctx.send_progress(1.0, "final message");
    }

    #[test]
    fn tool_call_context_with_progress_sender_forwards() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProgressReport>();
        let ctx = ToolCallContext::with_progress_sender(tx);
        ctx.send_progress(0.5, "halfway");

        // Drop ctx to close the sender, then drain
        drop(ctx);
        let reports: Vec<ProgressReport> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(reports.len(), 1, "Expected exactly 1 progress report");
        assert_eq!(reports[0].0, 0.5);
        assert_eq!(reports[0].2.as_deref(), Some("halfway"));
    }

    // ── Test helpers ───────────────────────────────────────────────────

    /// Insert a minimal symbol with a caller-controlled qualified name.
    fn insert_test_symbol_with_qname(
        store: &Store,
        file_id: FileId,
        simple_name: &str,
        qualified_name: &str,
        kind: atlas_engine::SymbolKind,
    ) {
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let sym = atlas_engine::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", simple_name, kind.as_str(), None),
            kind,
            name: simple_name.into(),
            qualified_name: qualified_name.into(),
            symbol_path: vec![simple_name.into()],
            file_id,
            language: atlas_engine::Language::TypeScript,
            range,
            name_range: range,
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
    }

    // ── Trace E2E tests ─────────────────────────────────────────────────

    /// Helper: insert a symbol with a custom qname and return its SymbolId
    /// for edge construction.
    fn insert_trace_test_symbol(
        store: &Store,
        file_id: FileId,
        simple_name: &str,
        qualified_name: &str,
        kind: atlas_engine::SymbolKind,
    ) -> SymbolId {
        let id =
            SymbolId::generate(&file_id, "typescript", simple_name, kind.as_str(), None);
        // Use the existing insert_test_symbol_with_qname to insert the symbol.
        // Reconstruct the same SymbolId to ensure edges refer to the correct id.
        insert_test_symbol_with_qname(store, file_id, simple_name, qualified_name, kind);
        id
    }

    // ── A. trace_callers E2E ────────────────────────────────────────────

    #[test]
    fn trace_callers_with_edge_returns_path() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        let caller_id = insert_trace_test_symbol(
            &store,
            file_a,
            "caller_func",
            "caller_func",
            atlas_engine::SymbolKind::Function,
        );
        let callee_id = insert_trace_test_symbol(
            &store,
            file_b,
            "callee_func",
            "callee_func",
            atlas_engine::SymbolKind::Function,
        );
        insert_test_edge(&store, caller_id, callee_id);

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({"symbol": "callee_func"});
        let (resp_str, is_error) = router.handle_trace_caller_path(&args);

        assert!(!is_error, "Expected no error, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(
            resp["kind"].as_str(),
            Some("trace_callers"),
            "Expected kind=trace_callers, got: {resp_str}"
        );
        // With an edge inserted, the result should have callers/path data.
        let ok = resp["ok"].as_bool().unwrap_or(false);
        assert!(ok, "Expected ok=true with callers data, got: {resp_str}");
    }

    #[test]
    fn trace_callers_ambiguous_symbol() {
        // BestEffortSingle picks the first candidate when multiple symbols
        // share the same qualified name.  The test verifies that a trace
        // succeeds rather than returning an ambiguous error.
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");
        let file_c = register_test_file(&store, "c.ts");

        insert_test_symbol_with_qname(
            &store,
            file_a,
            "turn",
            "turn",
            atlas_engine::SymbolKind::Function,
        );
        insert_test_symbol_with_qname(
            &store,
            file_b,
            "turn",
            "turn",
            atlas_engine::SymbolKind::Variable,
        );
        insert_test_symbol_with_qname(
            &store,
            file_c,
            "turn",
            "turn",
            atlas_engine::SymbolKind::Method,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({"symbol": "turn"});
        let (resp_str, is_error) = router.handle_trace_caller_path(&args);

        assert!(!is_error, "BestEffortSingle should pick a candidate, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(
            resp["kind"].as_str(),
            Some("trace_callers"),
            "Expected trace_callers response, got: {resp_str}"
        );
    }

    // ── B. trace_forward E2E ────────────────────────────────────────────

    #[test]
    fn trace_forward_with_edge_returns_path() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        let from_id = insert_trace_test_symbol(
            &store,
            file_a,
            "from_func",
            "from_func",
            atlas_engine::SymbolKind::Function,
        );
        let to_id = insert_trace_test_symbol(
            &store,
            file_b,
            "to_func",
            "to_func",
            atlas_engine::SymbolKind::Function,
        );
        insert_test_edge(&store, from_id, to_id);

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({"from": "from_func", "to": "to_func"});
        let (resp_str, is_error) = router.handle_trace_forward(&args);

        assert!(!is_error, "Expected no error, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(
            resp["kind"].as_str(),
            Some("trace_forward"),
            "Expected kind=trace_forward, got: {resp_str}"
        );
        let ok = resp["ok"].as_bool().unwrap_or(false);
        assert!(ok, "Expected ok=true with path data, got: {resp_str}");
    }

    #[test]
    fn trace_forward_ambiguous_to_path_aware() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");
        let file_c = register_test_file(&store, "c.ts");

        let from_id = insert_trace_test_symbol(
            &store,
            file_a,
            "from_func",
            "from_func",
            atlas_engine::SymbolKind::Function,
        );
        // Two "to_func" symbols — only one reachable from from_func
        let reachable_id = insert_trace_test_symbol(
            &store,
            file_b,
            "to_func",
            "to_func",
            atlas_engine::SymbolKind::Function,
        );
        insert_trace_test_symbol(
            &store,
            file_c,
            "to_func",
            "to_func",
            atlas_engine::SymbolKind::Function,
        );
        insert_test_edge(&store, from_id, reachable_id);

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({"from": "from_func", "to": "to_func"});
        let (resp_str, is_error) = router.handle_trace_forward(&args);

        assert!(
            !is_error,
            "Path-aware disambiguation should succeed, got: {resp_str}"
        );
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(
            resp["kind"].as_str(),
            Some("trace_forward"),
            "Expected kind=trace_forward, got: {resp_str}"
        );
    }

    #[test]
    fn trace_forward_ambiguous_to_no_reachable() {
        // BestEffortSingle picks the first 'to' candidate even without
        // path-aware disambiguation.  The trace_forward call succeeds
        // and returns no_path_found.
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");
        let file_c = register_test_file(&store, "c.ts");

        insert_trace_test_symbol(
            &store,
            file_a,
            "from_func",
            "from_func",
            atlas_engine::SymbolKind::Function,
        );
        // Two "to_func" symbols — neither reachable
        insert_trace_test_symbol(
            &store,
            file_b,
            "to_func",
            "to_func",
            atlas_engine::SymbolKind::Function,
        );
        insert_trace_test_symbol(
            &store,
            file_c,
            "to_func",
            "to_func",
            atlas_engine::SymbolKind::Function,
        );
        // No edge between them

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({"from": "from_func", "to": "to_func"});
        let (resp_str, is_error) = router.handle_trace_forward(&args);

        assert!(!is_error, "BestEffortSingle should pick candidate even without path, got: {resp_str}");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(
            resp["kind"].as_str(),
            Some("trace_forward"),
            "Expected trace_forward response"
        );
        // No path should be found between the two
        let diags = resp["diagnostics"].as_array().unwrap();
        let codes: Vec<&str> = diags
            .iter()
            .filter_map(|d| d["code"].as_str())
            .collect();
        assert!(
            codes.contains(&"no_path_found"),
            "Expected no_path_found code, got: {codes:?}"
        );
    }

    // ── C. trace_variable analysis_contract ────────────────────────────

    #[test]
    fn trace_variable_has_analysis_contract() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol_with_qname(
            &store,
            file_id,
            "main",
            "main",
            atlas_engine::SymbolKind::Function,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({
            "file_path": "test.ts",
            "line": 1,
            "column": 1,
        });
        let (resp_str, _is_error) = router.handle_trace_variable(&args);

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        // The response must include kind
        assert_eq!(
            resp["kind"].as_str(),
            Some("trace_variable"),
            "Expected kind=trace_variable, got: {resp_str}"
        );
        // Check for analysis_contract or lazy_diagnostics as evidence the
        // structural layer injected its metadata
        let has_contract = resp.get("analysis_contract").is_some();
        let has_lazy_diag = resp.get("lazy_diagnostics").is_some();
        assert!(
            has_contract || has_lazy_diag,
            "Expected analysis_contract or lazy_diagnostics field, got: {resp_str}"
        );
    }

    // ── E. Hex SymbolId resolution in callers ──────────────────────────

    #[test]
    fn trace_callers_hex_symbol_accepted() {
        // Hex strings are no longer auto-detected — they are treated as
        // qualified names. A hex-looking string won't match any symbol.
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        let sym_name = "my_func";
        let kind = atlas_engine::SymbolKind::Function;
        let sym_id =
            SymbolId::generate(&file_id, "typescript", sym_name, kind.as_str(), None);
        insert_test_symbol_with_qname(&store, file_id, sym_name, "my_func", kind);
        let hex_id = sym_id.to_hex();

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let args = serde_json::json!({"symbol": hex_id});
        let (resp_str, is_error) = router.handle_trace_caller_path(&args);

        assert!(
            resp_str.contains("not found") || is_error,
            "Hex string should not resolve as SymbolId: {resp_str}"
        );
    }

    #[test]
    fn trace_tool_has_per_kind_descriptions() {
        let tools = make_trace_tools();
        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name, "trace");

        let props = tool.input_schema.properties.as_ref().expect("should have properties");
        let kind = props.get("kind").expect("should have kind property");

        // Verify oneOf is present with 4 variants
        let one_of = kind.get("oneOf").expect("kind should have oneOf");
        let variants = one_of.as_array().expect("oneOf should be array");
        assert_eq!(variants.len(), 4);

        let descriptions: Vec<&str> = variants.iter()
            .map(|v| v.get("description").and_then(|d| d.as_str()).unwrap_or(""))
            .collect();

        assert!(descriptions[0].contains("position"), "point description missing: {:?}", descriptions[0]);
        assert!(descriptions[1].contains("dataflow"), "variable description should mention dataflow");
        assert!(descriptions[2].contains("call-graph"), "forward description should mention call-graph");
        assert!(descriptions[3].contains("call-graph"), "callers description should mention call-graph");
    }

    // ── Position-based symbol lookup tests ───────────────────────────

    /// Helper: insert a symbol with a specific source range.
    fn insert_symbol_with_range(
        store: &Store,
        file_id: FileId,
        name: &str,
        kind: atlas_engine::SymbolKind,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    ) -> atlas_engine::SymbolId {
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line,
            start_column: start_col,
            end_line,
            end_column: end_col,
        };
        let id = atlas_engine::SymbolId::generate(
            &file_id, "typescript", name, "function", None,
        );
        let sym = atlas_engine::SymbolDef {
            id,
            kind,
            name: name.into(),
            qualified_name: format!("{name}.{name}"),
            symbol_path: vec![name.into()],
            file_id,
            language: atlas_engine::Language::TypeScript,
            range,
            name_range: range,
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
        id
    }

    #[test]
    fn is_definition_kind_all_are_definitions() {
        // All currently defined SymbolKind variants are definitions.
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Function));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Class));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Struct));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Interface));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Enum));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::TypeAlias));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Variable));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Field));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Method));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Module));
        assert!(is_definition_kind(&atlas_engine::SymbolKind::Parameter));
    }

    #[test]
    fn position_lookup_picks_innermost_symbol() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/test.ts");

        // Outer: function at lines 1-10 (0-based: 0..9), cols 0-80
        insert_symbol_with_range(
            &store, file_id, "outer", atlas_engine::SymbolKind::Function,
            0, 0, 9, 80,
        );
        // Inner: function at lines 3-5 (0-based: 2..4), cols 0-40
        insert_symbol_with_range(
            &store, file_id, "inner", atlas_engine::SymbolKind::Function,
            2, 0, 4, 40,
        );

        let symbols = store.find_symbols_by_file(&file_id).unwrap();
        assert_eq!(symbols.len(), 2);

        // Position at line 4 (1-based) → 0-based line 3 should match inner
        let line_1based: u32 = 4;
        let target_line_0based = line_1based - 1;
        let target_col_0based: u32 = 1; // column 1 (ignored for line-only check)
        let inner_symbols: Vec<_> = symbols
            .iter()
            .filter(|s| {
                is_definition_kind(&s.kind)
                    && s.range.start_line <= target_line_0based
                    && target_line_0based <= s.range.end_line
                    && s.range.start_column <= target_col_0based
                    && target_col_0based <= s.range.end_column
            })
            .collect();
        assert_eq!(
            inner_symbols.len(),
            2,
            "both outer and inner cover line 4"
        );

        // Pick smallest range: (line_span, column_span)
        let mut sorted: Vec<_> = inner_symbols.iter().collect();
        sorted.sort_by_key(|s| {
            (s.range.end_line - s.range.start_line) * 1_000_000
                + (s.range.end_column - s.range.start_column)
        });
        assert_eq!(
            sorted[0].name, "inner",
            "Should pick innermost (smallest range) symbol"
        );
    }

    #[test]
    fn position_lookup_no_candidates_returns_empty() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/empty.ts");

        // Insert symbol at lines 1-3
        insert_symbol_with_range(
            &store, file_id, "func", atlas_engine::SymbolKind::Function,
            0, 0, 2, 10,
        );

        let symbols = store.find_symbols_by_file(&file_id).unwrap();
        assert_eq!(symbols.len(), 1);

        // Query line 10 (1-based) → no symbol should cover it
        let line_1based: u32 = 10;
        let target_line = line_1based - 1;
        let matches: Vec<_> = symbols
            .iter()
            .filter(|s| {
                is_definition_kind(&s.kind)
                    && s.range.start_line <= target_line
                    && target_line <= s.range.end_line
            })
            .collect();
        assert!(matches.is_empty(), "no symbol should cover line 10");
    }

    #[test]
    fn position_lookup_respects_column_filter() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/coltest.ts");

        // Two symbols on the same line span but different columns
        insert_symbol_with_range(
            &store, file_id, "left", atlas_engine::SymbolKind::Variable,
            2, 0, 2, 20,  // line 3 (0-based 2), cols 0-20
        );
        insert_symbol_with_range(
            &store, file_id, "right", atlas_engine::SymbolKind::Variable,
            2, 25, 2, 45, // line 3 (0-based 2), cols 25-45
        );

        let symbols = store.find_symbols_by_file(&file_id).unwrap();
        let target_line = 2; // 0-based line 2
        let target_col: u32 = 30; // col 30, should match "right" only

        let matches: Vec<_> = symbols
            .iter()
            .filter(|s| {
                is_definition_kind(&s.kind)
                    && s.range.start_line <= target_line
                    && target_line <= s.range.end_line
                    && s.range.start_column <= target_col
                    && target_col <= s.range.end_column
            })
            .collect();
        assert_eq!(matches.len(), 1, "only 'right' should match column 30");
        assert_eq!(matches[0].name, "right");
    }

    // ── merge_edge_deps tests ─────────────────────────────────────────────

    #[test]
    fn test_merge_edge_deps_empty() {
        let mut value = serde_json::json!({"dependents": []});
        let edge_deps = serde_json::json!([]);
        merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
        assert_eq!(value["dependents"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_merge_edge_deps_into_existing() {
        let mut value = serde_json::json!({
            "dependents": [{"file": "a.ts"}],
            "total_dependents": 1,
        });
        let edge_deps = serde_json::json!([
            {"file": "b.ts"},
            {"file": "c.ts"},
        ]);
        merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
        assert_eq!(value["dependents"].as_array().unwrap().len(), 3);
        assert_eq!(value["total_dependents"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_merge_edge_deps_into_empty() {
        let mut value = serde_json::json!({"dependents": []});
        let edge_deps = serde_json::json!([{"file": "b.ts"}]);
        merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
        assert_eq!(value["dependents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_merge_edge_deps_no_list_field() {
        let mut value = serde_json::json!({"other": 1});
        let edge_deps = serde_json::json!([{"file": "b.ts"}]);
        // Should not panic — gracefully skip
        merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
        assert_eq!(value["other"].as_u64().unwrap(), 1);
    }

    // ── handle_symbol_by_position view dispatch tests ───────────────────────

    #[test]
    fn handle_symbol_by_position_with_detail_view() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/test.ts");
        // Use insert_symbol_with_range to place symbol at 0-based line 0 so that
        // user's 1-based line=1 matches it.
        insert_symbol_with_range(
            &store, file_id, "myFunc",
            atlas_engine::SymbolKind::Function,
            0, 1, 0, 80,  // (start_line, start_col, end_line, end_col) 0-based
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({
            "file_path": "src/test.ts",
            "line": 1,
            "view": "detail"
        });

        let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
        assert!(!is_error, "Expected no error, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");
        // Detail view should have 'name' and 'qualified_name' fields
        assert!(resp.get("name").is_some(), "detail response should have 'name' field");
        assert!(resp.get("qualified_name").is_some(), "detail response should have 'qualified_name' field");
    }

    #[test]
    fn handle_symbol_by_position_with_context_view() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/main.rs");
        insert_symbol_with_range(
            &store, file_id, "process",
            atlas_engine::SymbolKind::Function,
            0, 1, 0, 80,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({
            "file_path": "src/main.rs",
            "line": 1,
            "view": "context"
        });

        let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
        assert!(!is_error, "Expected no error, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");
        // Context view should have 'subject' not 'name' at top level
        assert!(
            resp.get("subject").is_some(),
            "context response should have 'subject' field, got keys: {:?}",
            resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }

    #[test]
    fn handle_symbol_by_position_with_usages_view() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/lib.rs");
        insert_symbol_with_range(
            &store, file_id, "helper",
            atlas_engine::SymbolKind::Function,
            0, 1, 0, 80,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({
            "file_path": "src/lib.rs",
            "line": 1,
            "view": "usages"
        });

        let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
        assert!(!is_error, "Expected no error, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");
        // Usages view should have 'usages' array
        assert!(
            resp.get("usages").is_some(),
            "usages response should have 'usages' field, got keys: {:?}",
            resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
        assert!(
            resp.get("total_usages").is_some(),
            "usages response should have 'total_usages' field"
        );
    }

    #[test]
    fn handle_symbol_by_position_with_invalid_view() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/bad.rs");
        insert_symbol_with_range(
            &store, file_id, "func",
            atlas_engine::SymbolKind::Function,
            0, 1, 0, 80,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({
            "file_path": "src/bad.rs",
            "line": 1,
            "view": "nonexistent"
        });

        let (_resp_str, is_error) = router.handle_symbol(&ctx, &args);
        assert!(is_error, "Should return error for unknown view");
    }

    // ── Structured selector + view=detail filtering tests ──────────────

    #[test]
    fn structured_selector_detail_resolves_uniquely() {
        let store = test_store();
        // Two symbols with SAME qualified_name but DIFFERENT files.
        let file_a = register_test_file(&store, "src/a.ts");
        let file_b = register_test_file(&store, "src/b.ts");
        insert_symbol_with_range(
            &store, file_a, "Helper",
            atlas_engine::SymbolKind::Function,
            0, 1, 0, 80,
        );
        insert_symbol_with_range(
            &store, file_b, "Helper",
            atlas_engine::SymbolKind::Function,
            5, 1, 5, 80,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = ToolCallContext::empty();
        // Structured selector with file_path to disambiguate.
        let args = serde_json::json!({
            "symbol": {
                "qualified_name": "Helper.Helper",
                "file_path": "src/a.ts"
            },
            "view": "detail"
        });

        let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
        assert!(!is_error, "Expected unique resolution, got error: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");
        assert!(
            resp.get("name").is_some(),
            "detail response should have 'name' field, got: {resp_str}"
        );
        // Verify the resolved file matches the selector.
        assert_eq!(
            resp.get("file").and_then(|v| v.as_str()).unwrap_or(""),
            "src/a.ts",
            "Should resolve to the file specified in the selector, got: {resp_str}"
        );
    }

    #[test]
    fn plain_string_symbol_detail_still_works() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/main.ts");
        insert_symbol_with_range(
            &store, file_id, "SingleFunction",
            atlas_engine::SymbolKind::Function,
            0, 1, 0, 80,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({
            "symbol": "SingleFunction.SingleFunction",
            "view": "detail"
        });

        let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
        assert!(!is_error, "Expected no error, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("response should be valid JSON");
        assert!(
            resp.get("name").is_some(),
            "detail response should have 'name' field, got: {resp_str}"
        );
        assert_eq!(
            resp.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "SingleFunction",
            "Should resolve to SingleFunction, got: {resp_str}"
        );
    }

    // ── file_path diagnostic in ambiguous responses ──────────────────────

    #[test]
    fn invalid_file_path_in_selector_produces_diagnostic() {
        let store = test_store();
        let file_a = register_test_file(&store, "src/a.ts");
        let file_b = register_test_file(&store, "src/b.ts");

        // Two symbols with the same qualified name in different files → ambiguous.
        insert_test_symbol_with_qname(
            &store, file_a, "Foo", "Foo.Foo",
            atlas_engine::SymbolKind::Function,
        );
        insert_test_symbol_with_qname(
            &store, file_b, "Foo", "Foo.Foo",
            atlas_engine::SymbolKind::Function,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

        let args = serde_json::json!({
            "symbol": {
                "qualified_name": "Foo.Foo",
                "file_path": "src/nonexistent.ts"
            }
        });

        let (resp_str, is_error) = router.handle_symbol_detail(&args);
        assert!(is_error, "Expected error for ambiguous symbol, got: {resp_str}");
        assert!(
            resp_str.contains("file_path 'src/nonexistent.ts' does not match any file"),
            "Expected file_path diagnostic in error, got: {resp_str}"
        );
    }

    #[test]
    fn plain_string_ambiguous_no_false_diagnostic() {
        let store = test_store();
        let file_a = register_test_file(&store, "src/a.ts");
        let file_b = register_test_file(&store, "src/b.ts");

        // Two symbols with the same qualified name → ambiguous.
        insert_test_symbol_with_qname(
            &store, file_a, "Foo", "Foo.Foo",
            atlas_engine::SymbolKind::Function,
        );
        insert_test_symbol_with_qname(
            &store, file_b, "Foo", "Foo.Foo",
            atlas_engine::SymbolKind::Function,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

        let args = serde_json::json!({
            "symbol": "Foo.Foo"
        });

        let (resp_str, is_error) = router.handle_symbol_detail(&args);
        assert!(is_error, "Expected error for ambiguous symbol, got: {resp_str}");
        assert!(
            !resp_str.contains("does not match any file"),
            "Should NOT contain file_path diagnostic for plain string input, got: {resp_str}"
        );
    }

    #[test]
    fn valid_file_path_unambiguous_no_diagnostic_leak() {
        let store = test_store();
        let file_id = register_test_file(&store, "src/main.ts");

        insert_test_symbol_with_qname(
            &store, file_id, "MyFunc", "MyFunc.MyFunc",
            atlas_engine::SymbolKind::Function,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let args = serde_json::json!({
            "symbol": {
                "qualified_name": "MyFunc.MyFunc",
                "file_path": "src/main.ts"
            }
        });

        let (resp_str, is_error) = router.handle_symbol_detail(&args);
        assert!(!is_error, "Expected successful resolution, got error: {resp_str}");
        assert!(
            !resp_str.contains("does not match any file"),
            "Should NOT contain file_path diagnostic on successful resolution, got: {resp_str}"
        );
    }
}
