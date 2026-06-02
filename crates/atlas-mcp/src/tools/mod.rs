//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, lifecycle, branch_diff.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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

use crate::tools::query_snapshot::{InvestigationState, QUERY_SNAPSHOT_TTL_SECS, QuerySnapshot};

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
pub(crate) mod atlas_jobs;
pub(crate) mod branch_diff;
pub(crate) mod context;
pub(crate) mod dependencies;
pub(crate) mod dependents;
pub(crate) mod domain_rules;
pub(crate) mod graph;
pub(crate) mod index;
pub(crate) mod lazy_refresh;
pub(crate) mod lazy_response;
pub(crate) mod lifecycle;
pub(crate) mod open_project;
pub(crate) mod query_snapshot;
pub(crate) mod resume;
pub(crate) mod search;
pub(crate) mod status;
pub(crate) mod trace;
pub(crate) mod usages;
pub(crate) mod wait_for;

// -------------------------------------------------------------------
// StructuralEnsureOutcome
// -------------------------------------------------------------------

/// Result of lazy structural extraction triggered via
/// [`ensure_structural_for_files`] or [`ensure_structural_for_symbol_name`].
#[allow(dead_code)]
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
    pub(crate) lazy_service: LazyDataflowService,
    /// Graph engines built lazily on first request (after MCP handshake).
    pub(crate) search: Option<SearchEngine>,
    pub(crate) context: Option<ContextBuilder>,
    /// AST-aware source extractor (tree-sitter re-parsing).
    pub(crate) source_extractor: SourceExtractor,
    /// Project root directory for snippet extraction.
    pub(crate) project_root: std::path::PathBuf,
    tools: Vec<Tool>,
    /// Database/index signature at last graph build (used to detect external index/sync).
    last_graph_signature: String,
    /// True once the graph has been built at least once.
    graph_initialized: bool,
    /// Consolidates lazy graph refresh state: pending file IDs, cumulative
    /// write counter, and deferred full-rebuild scheduling.
    pub(crate) lazy_refresh_queue: Arc<lazy_refresh::LazyRefreshQueue>,
    /// Background-built graph waiting to be atomically swapped in.
    pending_graph_rebuild: Arc<Mutex<Option<Arc<atlas_engine::GraphEngine>>>>,
    /// Cached signature to avoid per-request COUNT queries.
    cached_signature: String,
    /// When the cached signature was last checked (avoids re-query within cooldown).
    last_signature_check: std::time::Instant,
    /// Cached result of `has_manual_full_index()` — detects a manually built
    /// (CLI) structural/full index vs MCP's automatic manifest-only index.
    /// `None` means not yet checked; checked lazily on first use.
    /// Uses Cell for interior mutability so &self methods (handle_index) can invalidate.
    cached_manual_full_index: Cell<Option<bool>>,
    /// Optional progress sender for long-running operations (set per-call in lib.rs).
    pub(crate) progress_sender: Option<ProgressSender>,
    /// Background task manager for `background: true` mode.
    pub(crate) task_manager: Arc<crate::task_manager::TaskManager>,
    /// Project activations prepared by background `open_project` tasks.
    pub(crate) pending_project_activations: Arc<Mutex<HashMap<String, PendingProjectActivation>>>,
    /// In-memory query snapshots for `atlas_resume`.
    pub(crate) query_snapshots: HashMap<String, QuerySnapshot>,
    /// Investigation state (MCP session scoped) for lazy job prioritization.
    pub(crate) investigation_state: InvestigationState,
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
        let source_extractor = SourceExtractor::new(store.clone(), project_root.clone());
        Self {
            store: store.clone(),
            lazy_service,
            search: Some(search),
            context: Some(context),
            source_extractor,
            project_root,
            tools: make_all_tools(),
            last_graph_signature: last_graph_signature.clone(),
            graph_initialized: true,
            lazy_refresh_queue: lazy_refresh::LazyRefreshQueue::new(),
            pending_graph_rebuild: Arc::new(Mutex::new(None)),
            cached_signature: last_graph_signature,
            last_signature_check: std::time::Instant::now(),
            progress_sender: None,
            task_manager: Arc::new(crate::task_manager::TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
            cached_manual_full_index: Cell::new(None),
            query_snapshots: HashMap::new(),
            investigation_state: InvestigationState::default(),
        }
    }

    /// Create a router without building the graph (fast startup).
    /// Graph is built lazily on the first request via `ensure_graph_initialized`.
    pub fn new_empty(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        let tools = make_all_tools();
        let lazy_service = LazyDataflowService::new(store.clone(), Some(project_root.clone()));
        let source_extractor = SourceExtractor::new(store.clone(), project_root.clone());
        Self {
            store: store.clone(),
            lazy_service,
            search: None,
            context: None,
            source_extractor,
            project_root,
            tools,
            last_graph_signature: String::new(),
            graph_initialized: false,
            lazy_refresh_queue: lazy_refresh::LazyRefreshQueue::new(),
            pending_graph_rebuild: Arc::new(Mutex::new(None)),
            cached_signature: String::new(),
            last_signature_check: std::time::Instant::now(),
            progress_sender: None,
            task_manager: Arc::new(crate::task_manager::TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
            cached_manual_full_index: Cell::new(None),
            query_snapshots: HashMap::new(),
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
        // Register AST-aware source extraction callback.
        let ext = self.source_extractor.clone();
        if let Some(ctx) = self.context.take() {
            self.context = Some(ctx.with_source_fn(Arc::new(move |id| ext.extract_source(id))));
        }
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

    /// Detect whether the current database was built from a manual (CLI) full
    /// structural index rather than MCP's automatic manifest-only index.
    ///
    /// MCP `index` always uses [`ExtractionMode::Manifest`]; the CLI
    /// `atlas index` (without `--analysis manifest`) builds a full structural
    /// index.  We detect this by checking whether a majority of indexed files
    /// have a `"structural"` layer with status `"complete"`.
    ///
    /// The result is cached for the lifetime of the session; callers that
    /// trigger a re-index (MCP `index` tool) should invalidate this cache
    /// after completion.
    pub(crate) fn has_manual_full_index(&self) -> bool {
        if let Some(cached) = self.cached_manual_full_index.get() {
            return cached;
        }
        let total = self.store.count_files().unwrap_or(0);
        if total == 0 {
            self.cached_manual_full_index.set(Some(false));
            return false;
        }
        let layer_counts = self
            .store
            .count_fresh_file_extraction_state()
            .unwrap_or_default();
        let structural_complete: usize = layer_counts
            .iter()
            .filter(|(l, s, _)| l == "structural" && s == "complete")
            .map(|(_, _, c)| *c as usize)
            .sum();
        // More than half of indexed files have structural layer — this is a
        // manual full index, not MCP's manifest-only index.
        let result = structural_complete > total / 2;
        self.cached_manual_full_index.set(Some(result));
        result
    }

    /// Invalidate the cached manual-full-index flag.
    ///
    /// Called after MCP `index` completes (which always produces a manifest
    /// index), so the next search/trace query re-checks the actual layer
    /// distribution.
    pub(crate) fn invalidate_manual_full_index_cache(&self) {
        self.cached_manual_full_index.set(None);
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
        self.lazy_service = LazyDataflowService::new(store.clone(), Some(project_root.clone()));
        self.source_extractor = SourceExtractor::new(store, project_root);
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

        // Step 1: Always flush pending incremental writes (no cooldown).
        // This ensures lazy writes from THIS request are visible before graph queries.
        let batch = self.lazy_refresh_queue.take_incremental_batch(500);
        self.refresh_graph_for_files(&batch)?;

        // Step 2: Deferred full rebuild — try to apply a background-built graph,
        // or spawn the rebuild thread. NEVER blocks the current request.
        self.try_apply_or_spawn_rebuild();

        // Step 3: Background preparse wrote to the DB — bypass cooldown.
        if self.lazy_refresh_queue.has_background_writes() {
            self.last_signature_check = self
                .last_signature_check
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or(self.last_signature_check);
        }

        // Step 4: 5s signature cache cooldown + rebuild if changed.
        if self.last_signature_check.elapsed().as_secs() < 5 {
            return Ok(());
        }
        self.last_signature_check = std::time::Instant::now();
        self.rebuild_if_signature_changed("Index signature changed, refreshing graph")
    }

    /// Force-refresh the graph snapshot regardless of cache cooldown.
    ///
    /// Called after lazy structural extraction writes new facts to the DB
    /// (via the context tool's tier-3 symbol resolution), so that the
    /// in-memory graph includes the newly parsed edges before graph-backed
    /// tools run their queries.
    pub(crate) fn force_refresh_graph(&mut self) -> anyhow::Result<()> {
        if !self.graph_initialized {
            return Ok(());
        }
        self.last_signature_check = std::time::Instant::now();
        self.rebuild_if_signature_changed("Force-refreshing graph after lazy structural extraction")
    }

    /// Atomically swap in a pre-built graph, updating both search and context engines.
    fn swap_graph(&mut self, graph: Arc<atlas_engine::GraphEngine>) {
        if let Some(ref mut s) = self.search {
            s.refresh_graph(Arc::clone(&graph));
        }
        if let Some(ref mut c) = self.context {
            c.refresh_graph(graph);
        }
        self.last_graph_signature = self.store.index_signature().unwrap_or_default();
        self.cached_manual_full_index.set(None);
    }

    /// Try to apply a background-built graph from the pending slot,
    /// or spawn a background rebuild thread if one was scheduled.
    ///
    /// Step 1: If a pending graph exists (built by a previous background thread),
    /// swap it in and clear the flags.
    /// Step 2: If a full rebuild was scheduled (cumulative threshold reached),
    /// and no rebuild is in progress, spawn a background thread to build the
    /// graph from the store. The current request continues with the old snapshot.
    fn try_apply_or_spawn_rebuild(&mut self) {
        // Step 1: Check for a pre-built graph in the pending slot.
        if let Some(graph) = self
            .pending_graph_rebuild
            .lock()
            .ok()
            .and_then(|mut p| p.take())
        {
            tracing::info!("Applying background-built graph snapshot");
            self.swap_graph(graph);
            self.lazy_refresh_queue.mark_rebuild_applied();
            self.lazy_refresh_queue.mark_rebuild_finished();
            return;
        }

        // Step 2: If a full rebuild is needed and no rebuild is in progress,
        // spawn a background thread to build the graph.
        if self.lazy_refresh_queue.needs_full_rebuild()
            && self.lazy_refresh_queue.try_start_rebuild()
        {
            tracing::info!("Spawning background full graph rebuild (non-blocking)");
            let store = Arc::clone(&self.store);
            let pending = Arc::clone(&self.pending_graph_rebuild);
            let queue = Arc::clone(&self.lazy_refresh_queue);
            std::thread::spawn(move || {
                match atlas_engine::GraphEngine::from_store(&store, 0.3) {
                    Ok(graph) => {
                        if let Ok(mut slot) = pending.lock() {
                            *slot = Some(Arc::new(graph));
                        }
                        // Note: rebuild_in_progress stays true until the pending
                        // graph is picked up by a subsequent try_apply_or_spawn_rebuild.
                    }
                    Err(e) => {
                        tracing::error!("Background graph rebuild failed: {:#}", e);
                        queue.mark_rebuild_finished();
                        queue.schedule_full_rebuild(); // retry on next call
                    }
                }
            });
        }
    }

    /// Refresh graph after lazy structural extraction.
    ///
    /// Uses per-file replace for small change sets: clones the existing
    /// in-memory snapshot, removes old nodes/edges for the changed files
    /// via [`remove_files_in_place`], then merges the fresh data from
    /// the store.  For large change sets (> 500 files), falls back to
    /// full rebuild (cloning the snapshot becomes costlier than SQLite scan).
    pub(crate) fn refresh_graph_for_files(&mut self, file_ids: &[FileId]) -> anyhow::Result<()> {
        if !self.graph_initialized || file_ids.is_empty() {
            return Ok(());
        }

        const REPLACE_THRESHOLD: usize = 500;
        if file_ids.len() > REPLACE_THRESHOLD {
            return self.force_refresh_graph();
        }

        // Clone the existing snapshot and replace changed files in-place
        let old_graph = match self.search.as_ref() {
            Some(s) => s.graph_snapshot(),
            None => return self.force_refresh_graph(),
        };
        let file_paths: std::collections::HashMap<FileId, String> = self
            .store
            .list_files()
            .unwrap_or_default()
            .into_iter()
            .map(|f| (f.file_id, f.path))
            .collect();

        let old_snap = old_graph.snapshot();
        let mut new_snapshot = old_snap.clone();
        new_snapshot.replace_files_in_place(&self.store, file_ids, 0.3, &file_paths)?;

        let new_graph = Arc::new(atlas_engine::GraphEngine::from_snapshot(new_snapshot));
        if let Some(ref mut s) = self.search {
            s.refresh_graph(Arc::clone(&new_graph));
        }
        if let Some(ref mut c) = self.context {
            c.refresh_graph(new_graph);
        }

        self.last_graph_signature = self.store.index_signature().unwrap_or_default();
        self.cached_manual_full_index.set(None);

        Ok(())
    }

    /// Rebuild the graph snapshot from the store if the index signature changed.
    fn rebuild_if_signature_changed(&mut self, reason: &str) -> anyhow::Result<()> {
        let current = self
            .store
            .index_signature()
            .unwrap_or_else(|_| self.cached_signature.clone());
        if current != self.last_graph_signature {
            tracing::info!("{reason}");
            let graph = Arc::new(atlas_engine::GraphEngine::from_store(&self.store, 0.3)?);
            if let Some(ref mut s) = self.search {
                s.refresh_graph(Arc::clone(&graph));
            }
            if let Some(ref mut c) = self.context {
                c.refresh_graph(graph);
            }
            self.last_graph_signature = current.clone();
            // Re-check whether a manual full index now exists (layer distribution
            // may have changed after external index/sync or lazy structural).
            self.cached_manual_full_index.set(None);
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
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let (result, is_error) = match name {
            "project" => self.handle_project(arguments),
            "index" => self.handle_index(arguments),
            "search" => self.handle_search(arguments),
            "symbol" => self.handle_symbol(arguments),
            "calls" => self.handle_calls(arguments),
            "explore" => self.handle_explore(arguments),
            "path" => self.handle_path(arguments),
            "impact" => self.handle_impact(arguments),
            "file_dependencies" => self.handle_file_dependencies(arguments),
            "trace" => self.handle_trace(arguments),
            "lifecycle" => self.handle_lifecycle(arguments),
            "branch_diff" => self.handle_branch_diff(arguments),
            "fp_dispatches" => self.handle_fp_dispatches(arguments),
            "domain_rules" => self.handle_domain_rules(arguments),
            "tasks" => self.handle_tasks(arguments),
            "task_status" => self.handle_task_status(arguments),
            "wait_for_task" => self.handle_wait_for_task(arguments),
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
        if self.has_manual_full_index() {
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
        );
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
        if self.has_manual_full_index() {
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
        );
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

    /// Resolve a qualified name to a SymbolId, returning error string on failure.
    /// When the store has no indexed files, the error includes guidance to run `index`.
    pub(crate) fn resolve_qname(&self, qname: &str) -> Result<SymbolId, String> {
        let symbols = self
            .store
            .find_symbols_by_qname(qname)
            .map_err(|e| format!("Lookup error: {e}"))?;
        match symbols.first() {
            Some(s) => Ok(s.id),
            None => {
                let mut err = format!("Symbol not found: {qname}");
                err.push_str(self.index_not_run_guidance());
                Err(err)
            }
        }
    }

    /// Resolve a qualified name to ALL matching SymbolIds (not just the first).
    ///
    /// In languages like C/C++, a symbol declared in a header (`.h`) and
    /// defined in a source file (`.c`) produces two SymbolIds that share the
    /// same qualified name but differ in `file_id`.  Call-graph edges
    /// connect the definition's SymbolId, so a `resolve_qname` that picks
    /// the header symbol will miss edges.  Callers that work with the graph
    /// snapshot should use this method and try each candidate.
    pub(crate) fn resolve_all_qname_symbols(&self, qname: &str) -> Result<Vec<SymbolId>, String> {
        let symbols = self
            .store
            .find_symbols_by_qname(qname)
            .map_err(|e| format!("Lookup error: {e}"))?;
        if symbols.is_empty() {
            let mut err = format!("Symbol not found: {qname}");
            err.push_str(self.index_not_run_guidance());
            return Err(err);
        }
        Ok(symbols.into_iter().map(|s| s.id).collect())
    }

    // -------------------------------------------------------------------
    // Query snapshot + investigation helpers
    // -------------------------------------------------------------------

    /// Generate a time-sortable query_id in format `q_{hex_ts_ms}_{hex_rand4}`.
    /// Uses an atomic counter to prevent collisions within the same millisecond.
    pub(crate) fn generate_query_id() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        // XOR ts components to spread bits, then mix with the atomic sequence
        let rand = (((ts >> 10) ^ (ts & 0xFFFF)) as u32 ^ seq ^ (seq.rotate_left(7))) as u16;
        format!("q_{ts:x}_{rand:04x}")
    }

    /// Store a query snapshot, pruning expired entries first.
    pub(crate) fn store_snapshot(&mut self, snapshot: QuerySnapshot) {
        self.prune_expired_snapshots();
        self.query_snapshots
            .insert(snapshot.query_id.clone(), snapshot);
    }

    /// Remove query snapshots older than TTL.
    pub(crate) fn prune_expired_snapshots(&mut self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(QUERY_SNAPSHOT_TTL_SECS);
        self.query_snapshots.retain(|_, s| s.created_at > cutoff);
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
            description: "Open, inspect, or list files in a project. Use action='open' to activate a project (never indexes), 'status' for a comprehensive overview including language capabilities and index mode, 'files' to list indexed files with language and parse status. Parameters for action='open': project_path (required), storage, scan_files, background. action='status' returns file/symbol/edge counts, extraction state, per-language capability profiles. action='files' supports optional limit, language, and path_prefix filters.".into(),
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
                        "enum": ["memory", "persistent"],
                        "description": "Storage mode: \"memory\" (in-memory, zero footprint, default) or \"persistent\" (project/.atlas/atlas.db)."
                    },
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
    ]
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
            description: "Get symbol information by qualified name (qname). view='detail' returns kind, location, signature, and caller/callee summaries (with optional source via includeCode). view='context' returns structured callers, callees, file peers, imports, dependencies, and precision tier. view='usages' returns reference usages. Default view is 'detail'.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "qname": { "type": "string", "description": "Fully qualified symbol name (primary parameter)." },
                    "view": {
                        "type": "string",
                        "enum": ["detail", "context", "usages"],
                        "description": "View mode: 'detail' for symbol info with optional source, 'context' for rich structured context, 'usages' for reference listing. Default: 'detail'."
                    },
                    "includeCode": { "type": "boolean", "description": "When true, includes the full source code of the enclosing definition (function/class/struct body). Default false (applies to view='detail' and 'context')." },
                    "limit": { "type": "integer", "description": "Max results for view='usages' (default 50)." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["qname".into()]),
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
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
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
            description: "Explore a symbol: detail info + all immediate neighbors grouped by edge kind. Returns shallow JSON (depth=1 adjacency).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
                    "includeCode": { "type": "boolean", "description": "When true, includes source code for the subject symbol. Default false." },
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
                    "from": { "type": "string", "description": "Source symbol qualified name" },
                    "to": { "type": "string", "description": "Target symbol qualified name" },
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
                    "symbol": { "type": "string", "description": "Qualified symbol name" },
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
                        "enum": ["point", "variable", "forward", "callers"],
                        "description": "Trace kind: 'point' for source position resolution, 'variable' for backward dataflow, 'forward' for forward call chain, 'callers' for backward call chain."
                    },
                    "file_id": { "type": "string", "description": "File ID in hex (alternative to file_path for kind='point'/'variable')." },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts'). Alternative to file_id." },
                    "line": { "type": "integer", "description": "1-based line number (required for kind='point'/'variable')." },
                    "column": { "type": "integer", "description": "1-based column number (required for kind='point'/'variable')." },
                    "symbol": { "type": "string", "description": "Qualified symbol name OR hex SymbolId (required for kind='callers'). Auto-detects format." },
                    "from": { "type": "string", "description": "Source qualified symbol name OR hex SymbolId (required for kind='forward'). Auto-detects format." },
                    "to": { "type": "string", "description": "Target qualified symbol name OR hex SymbolId (required for kind='forward'). Auto-detects format." },
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

// ── Annotation tools ─────────────────────────────────────────────────

fn make_annotation_tools() -> Vec<Tool> {
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
    tools.extend(make_annotation_tools());
    tools.extend(make_task_tools());
    tools
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
    /// Remaps `qname` → `qualified_name` (detail) or `qname` → `symbol` (context/usages).
    pub(crate) fn handle_symbol(&mut self, args: &Value) -> (String, bool) {
        let view = get_str(args, "view");
        let qname = get_str(args, "qname");
        if qname.is_empty() {
            return ("Missing required 'qname' parameter".to_string(), true);
        }

        match view {
            "detail" | "" => {
                // Remap: qname → qualified_name for the legacy detail handler
                let mut mapped = serde_json::Map::new();
                mapped.insert("qualified_name".into(), Value::String(qname.to_string()));
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_symbol_detail(&Value::Object(mapped))
            }
            "context" => {
                // Remap: qname → symbol for the legacy context handler
                let mut mapped = serde_json::Map::new();
                mapped.insert("symbol".into(), Value::String(qname.to_string()));
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_context(&Value::Object(mapped))
            }
            "usages" => {
                // Remap: qname → symbol for the legacy usages handler
                let mut mapped = serde_json::Map::new();
                mapped.insert("symbol".into(), Value::String(qname.to_string()));
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

    // ── calls ────────────────────────────────────────────────────────

    /// Handle `calls` tool — dispatch by `direction`/`depth`/`edge_kinds`.
    pub(crate) fn handle_calls(&mut self, args: &Value) -> (String, bool) {
        let direction = get_str(args, "direction");
        let depth = get_u64(args, "depth").unwrap_or(1);

        // Check if edge_kinds is non-default
        let edge_kinds: Vec<&str> = args
            .get("edge_kinds")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        // Treat empty edge_kinds as "all edges" (wildcard) per schema contract.
        // Only the explicit default set triggers the simple caller/callee fast path.
        let is_default_edges =
            !edge_kinds.is_empty() && edge_kinds == ["calls", "instantiates", "implements"];
        let is_custom_edges = !edge_kinds.is_empty() && !is_default_edges;

        // Custom edge_kinds, wildcard (empty), multi-hop, or bidirectional
        // → callgraph (handles all properly)
        if is_custom_edges
            || edge_kinds.is_empty()
            || depth > 1
            || direction == "both"
            || direction.is_empty()
        {
            // handle_callgraph internally defaults depth to 3, but our schema
            // default is 1. Inject depth when not user-specified.
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
            return self.handle_callgraph(&call_args);
        }

        match direction {
            "incoming" => self.handle_callers(args),
            "outgoing" => self.handle_callees(args),
            other => (
                format!("Unknown direction: '{other}'. Must be one of: incoming, outgoing, both"),
                true,
            ),
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

        // Resolve file_path to file_id for legacy handlers
        let clean = file_path.trim_start_matches("./").trim_start_matches('/');
        let file_id = match self.store.resolve_file_id(&self.project_root, clean) {
            Ok(Some(id)) => id,
            Ok(None) => return (format!("File not found: {file_path}"), true),
            Err(e) => return (format!("Failed to resolve file: {e}"), true),
        };

        let file_id_hex = file_id.to_hex();
        let mut mapped = serde_json::Map::new();
        mapped.insert("file_id".into(), Value::String(file_id_hex));
        if let Some(v) = args.get("limit") {
            mapped.insert("limit".into(), v.clone());
        }
        let mapped_args = Value::Object(mapped);

        let direction = get_str(args, "direction");
        match direction {
            "incoming" => self.handle_dependents(&mapped_args),
            "outgoing" | "" => self.handle_dependencies(&mapped_args),
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&mapped_args);
                let (in_str, in_err) = self.handle_dependents(&mapped_args);
                let result = json!({
                    "outgoing": serde_json::from_str::<Value>(&out_str).unwrap_or_default(),
                    "incoming": serde_json::from_str::<Value>(&in_str).unwrap_or_default(),
                });
                (
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                    out_err || in_err,
                )
            }
            other => (
                format!("Unknown direction: '{other}'. Must be one of: incoming, outgoing, both"),
                true,
            ),
        }
    }

    // ── trace ────────────────────────────────────────────────────────

    /// Handle `trace` tool — dispatch by `kind`.
    pub(crate) fn handle_trace(&mut self, args: &Value) -> (String, bool) {
        let kind = get_str(args, "kind");
        match kind {
            "point" => self.handle_trace_point(args),
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

        let (resp_str, _is_error) = router.handle_trace_point(&args);
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
            "qname": "test_func.test_func",
            "include_roots": ["/absolute/rejected"]
        });

        let (resp_str, is_error) = router.handle_symbol(&args);
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

        let (resp_str, is_error) = router.handle_context(&args);
        assert!(!is_error, "Expected success, got: {resp_str}");

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let warns = resp["warnings"].as_array();
        assert!(warns.is_some(), "Expected 'warnings' field in: {resp_str}");
        assert!(
            !warns.unwrap().is_empty(),
            "Expected non-empty warnings in: {resp_str}"
        );
    }
}
