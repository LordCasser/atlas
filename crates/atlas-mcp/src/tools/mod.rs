//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, capability.

/// File count after which cumulative lazy per-file replacements trigger
/// a synchronous full graph rebuild. Set at 80% of the 500-file threshold
/// where `refresh_graph_for_files` switches from per-file replace to
/// full rebuild anyway.
const CUMULATIVE_LAZY_REBUILD_THRESHOLD: usize = 400;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use atlas_engine::ContextBuilder;
use atlas_engine::FileId;
use atlas_engine::LazyBudget;
use atlas_engine::LazyCoordinator;
use atlas_engine::LazyDataflowService;
use atlas_engine::LazyStructuralService;
use atlas_engine::SearchEngine;
use atlas_engine::SourceExtractor;
use atlas_engine::Store;
use atlas_engine::SymbolId;
use atlas_engine::TraceDiagnostic;

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

pub(crate) mod annotations;
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
// StructuralEnsureOutcome
// -------------------------------------------------------------------

/// Result of lazy structural extraction triggered via
/// [`ensure_structural_for_files`] or [`ensure_structural_for_symbol_name`].
#[allow(dead_code)]
pub(crate) struct StructuralEnsureOutcome {
    pub warnings: Vec<String>,
    pub built_file_ids: Vec<atlas_engine::FileId>,
    pub precision_tier: atlas_engine::structs::precision::PrecisionTier,
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
    /// Set by background search when lazy structural writes new facts.
    /// Checked by [`maybe_refresh_graph`] to skip the 5-second cooldown.
    graph_stale_flag: Arc<AtomicBool>,
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
    /// Counter for cumulative lazy files built via `refresh_graph_for_files`.
    /// When this exceeds `CUMULATIVE_LAZY_REBUILD_THRESHOLD`, the next
    /// refresh triggers a full graph rebuild to keep the in-memory snapshot
    /// clean and avoid accumulated incremental-update overhead.
    cumulative_lazy_changes: usize,
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
            graph_stale_flag: Arc::new(AtomicBool::new(false)),
            cached_signature: last_graph_signature,
            last_signature_check: std::time::Instant::now(),
            progress_sender: None,
            task_manager: Arc::new(crate::task_manager::TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
            cached_manual_full_index: Cell::new(None),
            cumulative_lazy_changes: 0,
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
            graph_stale_flag: Arc::new(AtomicBool::new(false)),
            cached_signature: String::new(),
            last_signature_check: std::time::Instant::now(),
            progress_sender: None,
            task_manager: Arc::new(crate::task_manager::TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
            cached_manual_full_index: Cell::new(None),
            cumulative_lazy_changes: 0,
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
        // Background task(s) may have triggered lazy structural —
        // skip cooldown to pick up new facts immediately.
        if self.graph_stale_flag.swap(false, Ordering::AcqRel) {
            self.last_signature_check = self
                .last_signature_check
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or(self.last_signature_check);
        }
        // Cache signature for 5 seconds
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

        // Track cumulative lazy changes for progressive graph rebuild.
        // When enough files have been incrementally updated, do a full
        // synchronous rebuild to keep the in-memory snapshot clean.
        self.cumulative_lazy_changes += file_ids.len();
        if self.cumulative_lazy_changes >= CUMULATIVE_LAZY_REBUILD_THRESHOLD {
            tracing::info!(
                "Cumulative lazy changes reached {} (threshold {}), triggering full graph rebuild",
                self.cumulative_lazy_changes,
                CUMULATIVE_LAZY_REBUILD_THRESHOLD
            );
            self.force_refresh_graph()?;
            self.cumulative_lazy_changes = 0;
        }
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
        // No per-request graph rebuild — engines were initialized at startup.
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let (result, is_error) = match name {
            "index" => self.handle_index(arguments),
            "open_project" => self.handle_open_project(arguments),
            "status" => self.handle_status(),
            "jobs" => self.handle_jobs(),
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
            "annotate_fp_dispatch" => self.handle_annotate_fp_dispatch(arguments),
            "list_fp_annotations" => self.handle_list_fp_annotations(),
            "delete_fp_annotation" => self.handle_delete_fp_annotation(arguments),
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
    pub(crate) fn ensure_structural_for_files(
        &mut self,
        file_ids: impl IntoIterator<Item = FileId>,
        include_roots: Vec<atlas_engine::IncludeRoot>,
    ) -> StructuralEnsureOutcome {
        use atlas_engine::structs::precision::PrecisionTier;

        let mut warnings = Vec::new();
        let mut built_file_ids = Vec::new();
        let mut total_files_built = 0usize;
        let mut total_files_cached = 0usize;
        let mut total_budget_exceeded = false;
        if self.has_manual_full_index() {
            return StructuralEnsureOutcome {
                warnings,
                built_file_ids,
                precision_tier: PrecisionTier::Exact,
            };
        }
        // Deduplicate
        let file_set: HashSet<_> = file_ids.into_iter().collect();
        if file_set.is_empty() {
            return StructuralEnsureOutcome {
                warnings,
                built_file_ids,
                precision_tier: PrecisionTier::Exact,
            };
        }
        let coordinator =
            LazyCoordinator::with_project_root(self.store.clone(), self.project_root.clone())
                .with_include_roots(include_roots);
        let lazy = LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
        let mut budget = LazyBudget::structural();
        for file_id in &file_set {
            if !budget.can_continue() {
                total_budget_exceeded = true;
                break;
            }
            match coordinator.ensure_structural_with_closure(&lazy, file_id, &mut budget) {
                Ok((result, _job_id)) => {
                    total_files_built += result.files_built;
                    total_files_cached += result.files_cached;
                    total_budget_exceeded |= result.budget_exceeded;
                    built_file_ids.extend(result.built_file_ids);
                }
                Err(e) => {
                    warnings.push(format!(
                        "Lazy structural extraction failed for {}: {:#}",
                        file_id.to_hex(),
                        e
                    ));
                }
            }
        }
        if !built_file_ids.is_empty() {
            if let Err(e) = self.refresh_graph_for_files(&built_file_ids) {
                warnings.push(format!("Graph refresh failed: {:#}", e));
            }
        }
        let precision_tier = atlas_engine::precision::structural_precision(
            total_files_built,
            total_files_cached,
            total_budget_exceeded,
        );
        StructuralEnsureOutcome {
            warnings,
            built_file_ids,
            precision_tier,
        }
    }

    /// Ensure structural data for files containing a symbol name,
    /// with optional include_roots.  Useful for name-based lookups
    /// where the file_id is not yet known.
    pub(crate) fn ensure_structural_for_symbol_name(
        &mut self,
        symbol_name: &str,
        include_roots: Vec<atlas_engine::IncludeRoot>,
    ) -> StructuralEnsureOutcome {
        use atlas_engine::structs::precision::PrecisionTier;

        let mut warnings = Vec::new();
        let mut built_file_ids = Vec::new();
        let mut precision_tier = PrecisionTier::Unavailable;
        if self.has_manual_full_index() {
            return StructuralEnsureOutcome {
                warnings,
                built_file_ids,
                precision_tier: PrecisionTier::Exact,
            };
        }
        let mut budget = LazyBudget::structural();
        let coordinator =
            LazyCoordinator::with_project_root(self.store.clone(), self.project_root.clone())
                .with_include_roots(include_roots);
        let lazy = LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
        match coordinator.ensure_structural_for_symbol_with_closure(&lazy, symbol_name, &mut budget) {
            Ok(result) => {
                built_file_ids = result.built_file_ids;
                precision_tier = result.precision_tier;
            }
            Err(e) => {
                warnings.push(format!(
                    "Lazy structural extraction failed for '{}': {:#}",
                    symbol_name, e
                ));
            }
        }
        if !built_file_ids.is_empty() {
            if let Err(e) = self.refresh_graph_for_files(&built_file_ids) {
                warnings.push(format!("Graph refresh failed: {:#}", e));
            }
        }
        StructuralEnsureOutcome {
            warnings,
            built_file_ids,
            precision_tier,
        }
    }

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
            .map_err(|e| format!("Lookup error: {}", e))?;
        if symbols.is_empty() {
            let mut err = format!("Symbol not found: {}", qname);
            err.push_str(self.index_not_run_guidance());
            return Err(err);
        }
        Ok(symbols.into_iter().map(|s| s.id).collect())
    }

    /// Render a node from the graph snapshot to JSON.
    pub(crate) fn node_json(
        &self,
        snap: &atlas_engine::GraphSnapshot,
        ix: atlas_engine::NodeIx,
    ) -> Value {
        let n = snap.node(ix);
        json!({
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
            "file": self.resolve_file_path(&n.file_id),
            "line": n.start_line,
        })
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
            description: "Show project overview: file/symbol/edge counts, fresh extraction-state distribution, lazy dataflow stats, active extraction job count, DB stats, and per-language capability profiles.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "jobs".into(),
            description: "List active lazy extraction jobs. Use when a response reports pending lazy work or partial precision; retry the original query after the relevant job disappears.".into(),
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
            description: "Get detailed info for a symbol by qualified name: kind, location, signature, and caller/callee summaries (name + file + line). When includeCode is true, also returns the full source code of the enclosing definition (function/class/struct body) extracted via tree-sitter re-parsing.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "qualified_name": { "type": "string", "description": "Fully qualified symbol name" },
                    "includeCode": {
                        "type": "boolean",
                        "description": "When true, includes the full source code of the enclosing definition (function/class/struct body). Default false."
                    },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
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
            description: "Find the shortest path between two symbols through the graph (BFS). By default only follows call edges (calls, instantiates, implements, registers_callback). Use edge_kinds to override. Each edge hop now includes `direction` (forward/reverse) and `confidence`. The path also includes `breakpoints` describing indirect hops, test code contamination, and reversed edges. Use `prefer_production: true` to prefer paths through production code over test files.".into(),
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
                    "prefer_production": {
                        "type": "boolean",
                        "description": "When true, prefers paths through production (non-test) code. Test file nodes are deferred so production paths take priority even if longer by hop count. Default false."
                    },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\", \"instantiates\", \"implements\", \"registers_callback\"]. Use [] or [\"*\"] for all edge kinds."
                    },
                    "includeCode": {
                        "type": "boolean",
                        "description": "When true, includes source code for each node in the path. Default false."
                    },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
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
                    "includeCode": {
                        "type": "boolean",
                        "description": "When true, includes source code for the subject symbol. Default false."
                    },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "impact".into(),
            description: "Compute impact analysis: all symbols reachable from a given symbol (BFS bidirectionally — both downstream and upstream).".into(),
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
                    "includeCode": {
                        "type": "boolean",
                        "description": "When true, includes the subject symbol's full source code alongside markdown. Default false."
                    },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "trace_point".into(),
            description: "Resolve a source position to its full context: reference, symbol, data node, scope, bindings, and incident dataflow edges. Requires either file_id or file_path AND line AND column.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex (from files)" },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts')" },
                    "line": { "type": "integer", "description": "1-based line number" },
                    "column": { "type": "integer", "description": "1-based column number" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["line".into(), "column".into()]),
            },
        },
        Tool {
            name: "trace_variable".into(),
            description: "Trace where a variable's value comes from. Walks backward through dataflow edges from a source position to find origins (parameters, literals, globals). Requires either file_id or file_path AND line AND column.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_id": { "type": "string", "description": "File ID in hex (from files)" },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts')" },
                    "line": { "type": "integer", "description": "1-based line number" },
                    "column": { "type": "integer", "description": "1-based column number" },
                    "max_depth": { "type": "integer", "description": "Maximum backward traversal depth (default 30)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["line".into(), "column".into()]),
            },
        },
        Tool {
            name: "trace_caller_path".into(),
            description: "Trace how a function gets invoked. Walks backward through call edges (Calls/Instantiates/Implements) from a target symbol to its farthest caller. Requires either 'symbol' (hex ID) or 'symbol_name'.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Symbol ID in hex (from search or symbol)" },
                    "symbol_name": { "type": "string", "description": "Symbol name for lookup (e.g. 'inner'). Alternative to 'symbol' hex ID." },
                    "max_depth": { "type": "integer", "description": "Maximum backward call depth (default 20)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: None,
            },
        },
        Tool {
            name: "trace_forward".into(),
            description: "Trace the forward call chain from source to target. Answers 'how does A reach B?' by walking forward through call edges. Returns per-hop source snippets and edge types. Accepts either hex symbol IDs ('from'/'to') or symbol names ('from_name'/'to_name').".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "from": { "type": "string", "description": "Source symbol ID in hex" },
                    "to": { "type": "string", "description": "Target symbol ID in hex" },
                    "from_name": { "type": "string", "description": "Source symbol name (alternative to 'from' hex ID, e.g. 'main')" },
                    "to_name": { "type": "string", "description": "Target symbol name (alternative to 'to' hex ID, e.g. 'processRequest')" },
                    "max_depth": { "type": "integer", "description": "Maximum forward call depth (default 10)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: None,
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
        // ── Function-pointer dispatch annotations ──────────────────
        Tool {
            name: "annotate_fp_dispatch".into(),
            description: "Declare a function-pointer dispatch annotation for C/C++ code. Maps a struct's function-pointer field to its concrete target function, enabling the call-graph to trace through indirect calls. Example: annotate_fp_dispatch(field_qname='Curl_handler.do_it', target_qname='Curl_http'). Only valid for C and C++ — other languages use dynamic dispatch detected by static analysis.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "field_qname": { "type": "string", "description": "Qualified name of the function-pointer field in a struct (e.g., 'Curl_handler.do_it'). Must be a Field symbol." },
                    "target_qname": { "type": "string", "description": "Qualified name of the target function (e.g., 'Curl_http'). Must be a Function symbol." },
                    "confidence": { "type": "number", "description": "Confidence score 0.0-1.0 (default 1.0 for user-declared)." },
                })),
                required: Some(vec!["field_qname".into(), "target_qname".into()]),
            },
        },
        Tool {
            name: "list_fp_annotations".into(),
            description: "List all declared function-pointer dispatch annotations. Returns annotation_id, source/target qualified names, and confidence for each.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        },
        Tool {
            name: "delete_fp_annotation".into(),
            description: "Delete a function-pointer dispatch annotation. Requires either annotation_id OR field_qname. After deletion, the materialized edge is removed on next re-index.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "annotation_id": { "type": "string", "description": "Annotation ID (from list_fp_annotations)." },
                    "field_qname": { "type": "string", "description": "Qualified name of the function-pointer field (alternative to annotation_id)." },
                })),
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
            qualified_name: format!("{}.{}", name, name),
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
            .upsert_file_extraction_state(&file_id, "manifest", "hash1", "complete")
            .unwrap();

        let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let (resp_str, is_error) = router.handle_status();
        assert!(!is_error, "status failed: {}", resp_str);
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
        assert!(!is_error, "jobs failed: {}", resp_str);
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
            "Expected include_roots_warning code, got: {:?}",
            codes
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
            "Expected include_roots_warning, got: {:?}",
            codes
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
            "qualified_name": "test_func.test_func",
            "include_roots": ["/absolute/rejected"]
        });

        let (resp_str, is_error) = router.handle_symbol(&args);
        assert!(!is_error, "Expected success, got: {}", resp_str);

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let warns = resp["warnings"].as_array();
        assert!(
            warns.is_some(),
            "Expected 'warnings' field in: {}",
            resp_str
        );
        assert!(
            !warns.unwrap().is_empty(),
            "Expected non-empty warnings in: {}",
            resp_str
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
        assert!(!is_error, "Expected success, got: {}", resp_str);

        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
        let warns = resp["warnings"].as_array();
        assert!(
            warns.is_some(),
            "Expected 'warnings' field in: {}",
            resp_str
        );
        assert!(
            !warns.unwrap().is_empty(),
            "Expected non-empty warnings in: {}",
            resp_str
        );
    }
}
