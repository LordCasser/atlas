//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, lifecycle, branch_diff.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use atlas_engine::ContextBuilder;
use atlas_engine::FileId;
use atlas_engine::SearchEngine;
use atlas_engine::Store;
use atlas_engine::SymbolId;
use atlas_engine::TraceDiagnostic;
use atlas_engine::structs::SemanticConfidence;

use super::protocol::{CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema};

use serde_json::{Value, json};

use crate::tools::analysis_response::{WorkItem, WorkProgress, precision_to_view};
use crate::tools::lazy_response::{CapabilityStats, LazyResponse, SnapshotStore};
use crate::tools::runtime::graph_runtime::GraphMode;
use symbol_selector::{parse_symbol_input, SymbolInput};
use crate::tools::query_snapshot::QuerySnapshot;

use crate::tools::active_project::ActiveProject;
use crate::tools::tool_contract::{contract_for, ToolContract};

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

pub(crate) mod active_project;
pub(crate) mod analysis_response;
pub(crate) mod annotations;
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
pub(crate) mod runtime;
pub(crate) mod search;
pub(crate) mod status;
pub(crate) mod symbol_selector;
pub(crate) mod trace;
pub mod tool_contract;
pub(crate) mod usages;
pub(crate) mod wait_for;

/// Apply focus-aware envelope fields from a [`FocusResult`] to the LazyResponse.
///
/// Takes `&FocusResult` directly and merges precision, coverage, gaps,
/// and pending work items into the builder.
pub(crate) fn apply_focus_result_to_lr(
    lr: lazy_response::LazyResponse,
    result: &atlas_engine::focus::runtime::FocusResult,
) -> lazy_response::LazyResponse {
    let mut lr = lr;

    if result.mode == atlas_engine::focus::runtime::IndexMode::Focus {
        // ── Raw data ──
        if let Some(ref precision) = result.precision {
            lr = lr.with_precision(precision.clone());
        }
        if let Some(ref counts) = result.coverage_counts {
            lr = lr.with_coverage_counts(counts.clone());
        }
        if !result.gaps.is_empty() {
            lr = lr.with_gaps(result.gaps.clone());
        }

        // ── Analysis envelope ──
        if let Some(ref precision) = result.precision {
            let view = precision_to_view(precision);
            let state = if precision.confidence == SemanticConfidence::Certain {
                "ready"
            } else if precision.confidence == SemanticConfidence::High {
                "usable_partial"
            } else {
                "building"
            };
            lr = lr.with_analysis_state(state.to_string());
            lr = lr.with_analysis_scope("local".to_string());
            lr = lr.with_analysis_summary(format!(
                "scoped analysis: {} coverage, {} confidence",
                view.coverage, view.confidence
            ));
            lr = lr.with_analysis_next_action("use_result".to_string());
        } else if let Some(ref counts) = result.coverage_counts {
            lr = lr.with_analysis_state("building".to_string());
            lr = lr.with_analysis_scope("local".to_string());
            let total: usize = counts.values().sum();
            lr = lr.with_analysis_summary(format!(
                "partial results: {total} items across {} tiers",
                counts.len()
            ));
            lr = lr.with_analysis_next_action("use_result_or_wait_for_refinement".to_string());
        }
    }

    // ── Work items from pending closures ──
    if !result.pending_closure_ids.is_empty() {
        let items: Vec<WorkItem> = result
            .pending_closure_ids
            .iter()
            .map(|id| WorkItem {
                id: id.clone(),
                kind: "extraction".to_string(),
                state: "building".to_string(),
                scope: "local".to_string(),
                reason: "background_build".to_string(),
                progress: Some(WorkProgress { percent: 0 }),
                waitable: false,
                retry_after_ms: Some(2000),
            })
            .collect();
        lr = lr.with_work_items(items);
    }

    lr
}

// -------------------------------------------------------------------
// ToolRouter
// -------------------------------------------------------------------

/// Dispatches tools/list and tools/call.
pub struct ToolRouter {
    pub(crate) active: ActiveProject,
    tools: Vec<Tool>,
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
        let mut active = ActiveProject::new(store, project_root)
            .expect("Failed to construct ActiveProject");
        // Initialize graph state with pre-built search and context engines.
        active.graph_runtime.state.init_with(search, context);
        let mut router = Self { active, tools: make_all_tools() };
        router.init_focus();
        router
    }

    /// Create a router without building the graph (fast startup).
    /// Graph is built lazily on the first request via `ensure_graph_initialized`.
    pub fn new_empty(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        let active = ActiveProject::new(store, project_root)
            .expect("Failed to construct ActiveProject");
        let mut router = Self { active, tools: make_all_tools() };
        router.init_focus();
        router
    }

    /// Return the backing store.
    pub fn store(&self) -> Arc<Store> {
        self.active.store.clone()
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
        self.active.graph_runtime.ensure_initialized()?;
        Ok(())
    }

    /// Access the search engine.
    pub(crate) fn search_engine(&self) -> anyhow::Result<&SearchEngine> {
        self.active.graph_runtime.search_engine()
    }

    /// Access the context builder.
    pub(crate) fn context_builder(&self) -> anyhow::Result<&ContextBuilder> {
        self.active.graph_runtime.context_builder()
    }

    /// Check if the store has any indexed files (fast COUNT query).
    pub(crate) fn has_indexed_files(&self) -> bool {
        self.active.store.count_files().unwrap_or(0) > 0
    }

    /// Initialize the focus runtime for focus-driven lazy analysis.
    pub fn init_focus(&mut self) {
        let store = self.active.store.clone();
        let project_root = Some(self.active.root.clone());
        let mut runtime = atlas_engine::FocusRuntime::new(store, project_root);
        // Share AnalysisRuntime's LazyDataflowService to eliminate
        // double control plane (Finding #6).  The main closure engine
        // reuses this instance; the background scheduler still creates
        // its own for thread safety.
        runtime.with_lazy_dataflow(self.active.analysis_runtime.lazy_service.clone());
        self.active.query_runtime.focus_runtime = Some(Mutex::new(runtime));
    }

    /// Unified focus query preparation for focus-driven lazy analysis.
    ///
    /// Returns `(Some(FocusResult), warnings)` when focus analysis completed,
    /// or `(None, warnings)` when focus is not needed or unavailable.
    pub fn prepare_focus_query(
        &mut self,
        intent: Option<atlas_engine::QueryIntent>,
    ) -> (Option<atlas_engine::focus::runtime::FocusResult>, Vec<String>) {
        // 1. Full index already exists — no focus needed.
        if self.active.query_runtime.cache.has_manual_full_index(&self.active.store) {
            return (None, vec![]);
        }

        // 2. No intent — nothing to prepare.
        let intent = match intent {
            Some(i) => i,
            None => return (None, vec![]),
        };

        // 3. Delegate FocusRuntime interaction to QueryRuntime.
        let (focus_result, warnings) =
            self.active.query_runtime.prepare(&intent, &self.active.store);

        // 4. Post-processing: record lazy writes and refresh graph.
        if let Some(ref result) = focus_result {
            if !result.built_files.is_empty() {
                self.active
                    .query_runtime
                    .lazy_refresh_queue
                    .record_lazy_writes(&result.built_files);
            }
            if let Err(e) = self.maybe_refresh_graph() {
                let mut combined = warnings.clone();
                combined.push(format!("Focus succeeded but graph refresh failed: {e}"));
                return (Some(result.clone()), combined);
            }
        }

        (focus_result, warnings)
    }

    /// Query the DB for real capability file counts.
    /// Returns None if the query fails (graceful degradation).
    /// Reserved for future status/capabilities reporting endpoints.
    #[allow(dead_code)]
    pub(crate) fn get_capability_stats(&self) -> Option<CapabilityStats> {
        let (files_with_dataflow, files_structural_only, files_manifest_only, files_with_cfg) =
            self.active.store.get_capability_counts().ok()?;
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

    /// Inject graph edge provenance into the response JSON when the graph
    /// was built from a partial/closure-based index (FocusPartial mode).
    ///
    /// Focus precision from lazy extraction takes priority when present
    /// (LazyResponse may overwrite this with per-query coverage data).
    pub(crate) fn inject_graph_precision(&self, resp: &mut serde_json::Value) {
        let precision = self.active.graph_runtime.precision_info();
        if precision.mode == GraphMode::FocusPartial {
            // Only inject graph-level precision if no per-query (focus) precision
            // is already present. Focus precision provides richer per-query
            // coverage detail that should not be overwritten.
            if resp.get("precision").is_none() {
                resp["precision"] = json!({
                    "mode": "focus_partial",
                    "initialized": precision.initialized,
                    "edge_count": precision.edge_count,
                });
            }
        }
    }

    /// Rebuild the graph snapshot from the store if the index signature changed.
    fn rebuild_if_signature_changed(&mut self, reason: &str) -> anyhow::Result<()> {
        let current = self
            .active.store
            .index_signature()
            .unwrap_or_else(|_| self.active.query_runtime.cache.cached_signature.clone());
        if current != self.active.graph_runtime.state.last_graph_signature {
            tracing::info!("{reason}");
            let graph = Arc::new(atlas_engine::GraphEngine::from_store(&self.active.store, 0.3)?);
            if let Some(ref mut s) = self.active.graph_runtime.state.search {
                s.refresh_graph(Arc::clone(&graph));
            }
            if let Some(ref mut c) = self.active.graph_runtime.state.context {
                c.refresh_graph(graph);
            }
            self.active.graph_runtime.state.last_graph_signature = current.clone();
            // Re-check whether a manual full index now exists (layer distribution
            // may have changed after external index/sync or lazy structural).
            *self.active.query_runtime.cache.cached_manual_full_index.write().unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.active.query_runtime.cache.cached_signature = current;
        Ok(())
    }

    /// Resolve a [`FileId`] to its human-readable file path.
    /// Delegates to [`StoreQueryRuntime::resolve_file_path`].
    pub(crate) fn resolve_file_path(&self, file_id: &FileId) -> String {
        self.active.store_query_runtime.resolve_file_path(file_id)
    }

    /// Switch the active project to a new store+root, clearing graph/cache state.
    ///
    /// This is the core mechanism for `atlas_open_project` and project switching.
    /// After activation, the next graph-backed tool call will lazily rebuild the
    /// snapshot from the new store.
    pub(crate) fn activate_project(&mut self, project_root: std::path::PathBuf, store: Arc<Store>) {
        self.active = ActiveProject::new(store, project_root)
            .expect("Failed to construct ActiveProject during project activation");
        self.init_focus();
    }

    /// Activate a prepared background `open_project` result, if one exists.
    pub(crate) fn activate_pending_project_for_task(&mut self, task_id: &str) -> Option<String> {
        let pending = self
            .active.job_runtime.pending_project_activations
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
    /// **Refresh responsibility**: this method is called internally by
    /// `prepare_focus_query` whenever new files were built. Callers that
    /// use that helper do **not** need to call `maybe_refresh_graph` separately.
    ///
    /// Callers that modify the store independently (e.g. through a full re-index
    /// signal) may still need to call this to pick up changes.
    pub(crate) fn maybe_refresh_graph(&mut self) -> anyhow::Result<()> {
        if !self.active.graph_runtime.state.graph_initialized {
            return Ok(());
        }

        // Step 1: Always flush pending incremental writes (no cooldown).
        // This ensures lazy writes from THIS request are visible before graph queries.
        let batch = self.active.query_runtime.lazy_refresh_queue.take_incremental_batch(500);
        self.active.graph_runtime.state.refresh_graph_for_files(&self.active.store, &batch)?;
        // Cache invalidation: new store data may have changed layer distribution.
        if !batch.is_empty() {
            *self.active.query_runtime.cache.cached_manual_full_index.write().unwrap_or_else(|e| e.into_inner()) = None;
        }

        // Step 2: Deferred full rebuild — try to apply a background-built graph,
        // or spawn the rebuild thread. NEVER blocks the current request.
        self.active.graph_runtime.state.try_apply_or_spawn_rebuild(
            Arc::clone(&self.active.store),
            Arc::clone(&self.active.query_runtime.lazy_refresh_queue),
        );

        // Step 3: Always check the store signature. A full index may change
        // extraction layers and graph facts without going through this router.
        self.active.query_runtime.cache.last_signature_check = std::time::Instant::now();
        self.rebuild_if_signature_changed("Index signature changed, refreshing graph")
    }

    /// Force-refresh the graph snapshot regardless of cache cooldown.
    ///
    /// Called after lazy structural extraction writes new facts to the DB
    /// (via the context tool's tier-3 symbol resolution), so that the
    /// in-memory graph includes the newly parsed edges before graph-backed
    /// tools run their queries.
    pub(crate) fn force_refresh_graph(&mut self) -> anyhow::Result<()> {
        if !self.active.graph_runtime.state.graph_initialized {
            return Ok(());
        }
        self.active.query_runtime.cache.last_signature_check = std::time::Instant::now();
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
    /// Graph initialization and signature-refresh are performed here as
    /// resource preparation *before* the contract dispatch, so the
    /// contract determines what resources are needed.  The MCP server layer
    /// ([`AtlasMcpService::call_tool`]) delegates entirely to this method.
    pub fn call_tool(
        &mut self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: &Value,
    ) -> CallToolResult {
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let contract = contract_for(name, arguments);

        // Phase 7a: Resource preparation based on contract.
        //
        // Graph-backed tools need the graph snapshot initialized and
        // refreshed before dispatch.  Doing this inside call_tool() means the
        // contract itself determines what resources are needed, removing the
        // need for the MCP server layer to pre-check tool_call_requires_graph().
        if Self::tool_call_requires_graph(name, arguments) {
            if let Err(e) = self.ensure_graph_initialized() {
                return CallToolResult {
                    content: vec![ContentBlock::text(format!(
                        "Failed to initialize graph snapshot: {e:#}"
                    ))],
                    is_error: Some(true),
                };
            }
            if let Err(e) = self.maybe_refresh_graph() {
                return CallToolResult {
                    content: vec![ContentBlock::text(format!(
                        "Failed to refresh graph snapshot: {e:#}"
                    ))],
                    is_error: Some(true),
                };
            }
        }

        let (mut result, is_error) = match contract {
            ToolContract::ProjectLifecycle => self.handle_project(arguments),
            ToolContract::StatusRead => self.dispatch_status_read(ctx, name, arguments),
            ToolContract::ExplicitIndexBuild => self.handle_index(ctx, arguments),
            ToolContract::SemanticGraphQuery(_) => self.dispatch_graph_query(ctx, name, arguments),
            ToolContract::TraceQuery(_) => self.dispatch_trace_query(ctx, name, arguments),
            ToolContract::StoreFactQuery(_) => self.dispatch_store_query(ctx, name, arguments),
            ToolContract::SemanticAnalysis(_) => self.dispatch_analysis(ctx, name, arguments),
            ToolContract::OverlayMutation(_) | ToolContract::OverlayRead => {
                self.dispatch_overlay(ctx, name, arguments)
            }
            ToolContract::TaskControl => self.dispatch_task_control(ctx, name, arguments),
        };

        // Phase 9: Auto-inject graph precision for SemanticGraphQuery tools.
        // This replaces the 8 manual inject_graph_precision() calls that were
        // scattered across graph.rs and context.rs.  Focus precision (per-query
        // coverage data from lazy extraction) takes priority — graph precision
        // is only injected when no per-query precision exists.
        if !is_error && matches!(contract, ToolContract::SemanticGraphQuery(_)) {
            if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&result) {
                self.inject_graph_precision(&mut val);
                result = serde_json::to_string_pretty(&val).unwrap_or(result);
            }
        }

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

    /// Sub-dispatcher: `StatusRead` contract tools.
    fn dispatch_status_read(
        &mut self,
        _ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "project" => self.handle_project(args),
            _ => (format!("Unknown tool: {name}"), true),
        }
    }

    /// Sub-dispatcher: `SemanticGraphQuery` contract tools.
    fn dispatch_graph_query(
        &mut self,
        ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "calls" => self.handle_calls(args),
            "explore" => self.handle_explore(args),
            "path" => self.handle_path(args),
            "impact" => self.handle_impact(args),
            "symbol" => self.handle_symbol(ctx, args),
            _ => (format!("Unknown graph tool: {name}"), true),
        }
    }

    /// Sub-dispatcher: `TraceQuery` contract tools.
    fn dispatch_trace_query(
        &mut self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: &Value,
    ) -> (String, bool) {
        let kind = arguments.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match (name, kind) {
            ("trace", "point") => self.handle_trace_point(ctx, arguments),
            ("trace", "variable") => self.handle_trace_variable(arguments),
            ("trace", "forward") => self.handle_trace_forward(arguments),
            ("trace", "callers") => self.handle_trace_caller_path(arguments),
            ("trace", "") => self.handle_trace_point(ctx, arguments), // default: point
            _ => (format!("Unknown trace kind: {kind}"), true),
        }
    }

    /// Sub-dispatcher: `StoreFactQuery` contract tools.
    fn dispatch_store_query(
        &mut self,
        ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "symbol" => self.handle_symbol(ctx, args),
            "search" => self.handle_search(ctx, args),
            "file_dependencies" => self.handle_file_dependencies(args),
            _ => (format!("Unknown store query tool: {name}"), true),
        }
    }

    /// Sub-dispatcher: `SemanticAnalysis` contract tools.
    fn dispatch_analysis(
        &mut self,
        _ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "branch_diff" => self.handle_branch_diff(args),
            "lifecycle" => self.handle_lifecycle(args),
            _ => (format!("Unknown analysis tool: {name}"), true),
        }
    }

    /// Sub-dispatcher: `OverlayMutation` / `OverlayRead` contract tools.
    fn dispatch_overlay(
        &mut self,
        _ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "fp_dispatches" => self.handle_fp_dispatches(args),
            "domain_rules" => self.handle_domain_rules(args),
            _ => (format!("Unknown overlay tool: {name}"), true),
        }
    }

    /// Sub-dispatcher: `TaskControl` contract tools.
    fn dispatch_task_control(
        &mut self,
        _ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "tasks" => self.handle_tasks(args),
            "task_status" => self.handle_task_status(args),
            "wait_for_task" => self.handle_wait_for_task_sync(args),
            "resume_task" => self.handle_resume_task(args),
            _ => (format!("Unknown task tool: {name}"), true),
        }
    }

    pub(crate) fn handle_task_status(&mut self, args: &serde_json::Value) -> (String, bool) {
        let task_id = get_str(args, "task_id");
        if task_id.is_empty() {
            return ("Missing task_id parameter".to_string(), true);
        }
        match self.active.job_runtime.task_manager.get_task(task_id) {
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
            if !self.active.root.join(&normalized).is_dir() {
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
        self.active.job_runtime.prune_expired_snapshots();
        self.active.job_runtime.query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(snapshot.query_id.clone(), snapshot);
    }

    /// Update or create investigation based on a tool call focus.
    pub(crate) fn update_investigation(&mut self, focus: atlas_engine::InvestigationFocus) {
        self.active.job_runtime.investigation_state.update(focus);
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
    /// Delegates to [`StoreQueryRuntime::read_symbol_source`].
    ///
    /// Returns `None` if the file cannot be found, is outside the project
    /// root, or the symbol range is invalid.  Callers should silently omit
    /// the `source` field when this returns `None`.
    pub(crate) fn read_symbol_source(&self, symbol_id: &SymbolId) -> Option<String> {
        self.active.store_query_runtime.read_symbol_source(symbol_id)
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
        let file_id = match self.active.store.resolve_file_id(&self.active.root, clean) {
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
        let mut _capability_mask = atlas_engine::structs::CapabilityMask::default();
        let mut _coverage = "full";
        let mut _reason: Option<&str> = None;

        if !self.active.query_runtime.cache.has_manual_full_index(&self.active.store) {
            let max_files = get_u64(args, "max_structural_files")
                .or_else(|| get_u64(args, "limit"))
                .unwrap_or(50) as usize;
            let _file_ids = match direction {
                "incoming" | "both" => {
                    let (candidates, truncated) =
                        self.collect_edge_dependent_file_ids(&[file_id], max_files);
                    let mut result = vec![file_id];
                    result.extend(candidates);
                    if truncated {
                        _coverage = "partial";
                        _reason = Some("candidate_limit_exceeded");
                    }
                    result
                }
                _ => vec![file_id],
            };
            let (focus_result, focus_warnings) = self.prepare_focus_query(None);
            lazy_warnings = focus_warnings;
            built_file_count = focus_result.as_ref().map(|r| r.built_files.len()).unwrap_or(0);
        } else {
            _capability_mask = self.active.store.derive_capability_for_files(&[file_id]);
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
                let body = serde_json::from_str::<Value>(&out).unwrap_or_default();
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {} files (structural mode)", built_file_count)
                } else {
                    "Full index available".into()
                };
                LazyResponse::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err)
                    .with_analysis_state("ready".into())
                    .with_analysis_scope("structural".into())
                    .with_analysis_summary(summary)
                    .with_analysis_next_action("use_result".into())
                    .build(body, self)
            }
            "outgoing" | "" => {
                let (out, err) = self.handle_dependencies(&mapped_args);
                let body = serde_json::from_str::<Value>(&out).unwrap_or_default();
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {} files (structural mode)", built_file_count)
                } else {
                    "Full index available".into()
                };
                LazyResponse::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err)
                    .with_analysis_state("ready".into())
                    .with_analysis_scope("structural".into())
                    .with_analysis_summary(summary)
                    .with_analysis_next_action("use_result".into())
                    .build(body, self)
            }
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&mapped_args);
                let (in_str, in_err) = self.handle_dependents(&mapped_args);
                let body = json!({
                    "outgoing": serde_json::from_str::<Value>(&out_str).unwrap_or_default(),
                    "incoming": serde_json::from_str::<Value>(&in_str).unwrap_or_default(),
                });
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {} files (structural mode)", built_file_count)
                } else {
                    "Full index available".into()
                };
                let err = out_err || in_err;
                LazyResponse::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err)
                    .with_analysis_state("ready".into())
                    .with_analysis_scope("structural".into())
                    .with_analysis_summary(summary)
                    .with_analysis_next_action("use_result".into())
                    .build(body, self)
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
            let syms = match self.active.store.find_symbols_by_file(fid) {
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
        let edges = match self.active.store.find_edges_for_files(target_file_ids) {
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
        let symbols = match self.active.store.find_symbols_by_ids(&ids_vec) {
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
        let our_symbols = match self.active.store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.active.store.find_edges_for_files(&[*file_id]) {
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
        let symbols = match self.active.store.find_symbols_by_ids(&ids_vec) {
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
        let our_symbols = match self.active.store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.active.store.find_edges_for_files(&[*file_id]) {
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
        let symbols = match self.active.store.find_symbols_by_ids(&ids_vec) {
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
        let wfr = wait_for::handle_wait_for_task_sync(&self.active.job_runtime.task_manager, args);
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
        if self.active.store.get_file(&file_id).ok().flatten().is_none() {
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
        let (_include_roots, root_warnings) = self.include_roots_from_args(args);
        let (_focus_result, focus_warnings) = self.prepare_focus_query(None);
        let mut warnings: Vec<String> = root_warnings;
        warnings.extend(focus_warnings);

        // Find all symbols in the file
        let symbols = match self.active.store.find_symbols_by_file(&file_id) {
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

    // ── ensure_graph_initialized mode detection ───────────────────────

    #[test]
    fn ensure_graph_initialized_detects_focus_partial_mode() {
        let store = test_store();
        // In-memory store with no index → read_index_mode returns empty/default,
        // which is not a rich index mode → FocusPartial.
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        assert_eq!(
            router.active.graph_runtime.mode,
            GraphMode::FocusPartial,
            "fresh in-memory store should produce FocusPartial mode"
        );
    }

    #[test]
    fn ensure_graph_initialized_detects_full_canonical_mode() {
        let store = test_store();
        // Register a file with a "structural" extraction state so
        // read_index_mode() returns a rich index mode.
        let file_id = register_test_file(&store, "test.ts");
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "hash1",
                "complete",
                atlas_engine::structs::CapabilityMask::default(),
            )
            .unwrap();

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        assert_eq!(
            router.active.graph_runtime.mode,
            GraphMode::FullCanonical,
            "store with structural extraction should produce FullCanonical mode"
        );
    }

    // ── Phase 7a: resource preparation inside call_tool ────────────────

    /// A graph tool call on a store without schema should fail inside
    /// call_tool() with is_error=true and a descriptive message.
    #[test]
    fn graph_init_error_propagates_in_call_tool() {
        // Store without schema → GraphEngine::from_store will fail
        let store = Store::open_in_memory().unwrap();
        let mut router = ToolRouter::new_empty(Arc::new(store), PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({"symbol": "foo.bar"});
        let result = router.call_tool(&ctx, "calls", &args);

        assert_eq!(result.is_error, Some(true), "should be an error");
        let body = &result.content[0];
        let text = match body {
            ContentBlock::Text { text } => text,
        };
        assert!(
            text.contains("Failed to initialize graph snapshot"),
            "error text should mention graph init failure, got: {text}",
        );
    }

    /// A non-graph tool call should NOT trigger graph initialization,
    /// even if the store has no schema (which would cause graph init to
    /// fail were it attempted).
    #[test]
    fn call_tool_without_graph_init_for_non_graph_tool() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        assert!(
            !router.active.graph_runtime.state.graph_initialized,
            "graph should not be initialized yet",
        );

        let ctx = ToolCallContext::empty();
        // "domain_rules" with no action → OverlayRead → non-graph tool
        let args = serde_json::json!({});
        let _result = router.call_tool(&ctx, "domain_rules", &args);

        assert!(
            !router.active.graph_runtime.state.graph_initialized,
            "graph should still NOT be initialized after a non-graph tool call",
        );
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
        // analysis block must be present (unified envelope)
        let analysis = &resp["analysis"];
        assert!(
            analysis.get("state").is_some(),
            "analysis block missing state field: {resp_str}"
        );
        assert_eq!(analysis["state"], "ready");
        assert_eq!(analysis["scope"], "structural");
        assert!(
            analysis.get("summary").is_some(),
            "analysis block missing summary field: {resp_str}"
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
        // Check for the analysis block as evidence the
        // envelope injected its metadata
        let has_analysis = resp.get("analysis").is_some();
        let has_query_id = resp.get("query_id").is_some();
        assert!(
            has_analysis || has_query_id,
            "Expected analysis or query_id field, got: {resp_str}"
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

    // ── Focus runtime tests ───────────────────────────────────────────

    #[test]
    fn init_focus_sets_up_runtime() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        // After construction, focus_runtime is already initialized
        // (new_empty calls init_focus).
        assert!(router.active.query_runtime.focus_runtime.is_some(), "focus_runtime should be Some after construction");

        // Calling init_focus again is idempotent.
        router.init_focus();
        assert!(
            router.active.query_runtime.focus_runtime.is_some(),
            "focus_runtime should remain Some after init_focus()"
        );
    }

    #[test]
    fn focus_runtime_initialized_on_activate_project() {
        let store = test_store();
        let store2 = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        // Before activation, focus_runtime is Some (initialized by new_empty).
        assert!(router.active.query_runtime.focus_runtime.is_some());

        // Simulate project switch — activate_project creates a fresh ActiveProject
        // and then calls init_focus(), so focus_runtime is re-initialized.
        router.activate_project(PathBuf::from("/other"), store2);
        assert!(
            router.active.query_runtime.focus_runtime.is_some(),
            "focus_runtime should be Some after project activation (activate_project calls init_focus)"
        );
    }

    #[test]
    fn init_focus_shares_lazy_dataflow_service() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        // init_focus is called in new_empty, so focus_runtime should exist
        assert!(router.active.query_runtime.focus_runtime.is_some(),
            "focus_runtime should be initialized");

        // The shared_lazy_dataflow field is not publicly accessible,
        // but we can verify that prepare_focus_query works correctly.
        let intent = Some(atlas_engine::QueryIntent::Calls {
            symbol_name: "test".into(),
            file_id: None,
            symbol_id: None,
        });
        let (_result, warnings) = router.prepare_focus_query(intent);
        // Should not crash; shared dataflow service is used internally
        assert!(warnings.iter().all(|w| !w.contains("panic") && !w.contains("unwrap")),
            "warnings should not contain panics: {:?}", warnings);
    }

    // ── apply_focus_result_to_lr work items tests ────────────────────

    /// Mock SnapshotStore that captures stored snapshots in a Vec.
    struct MockSnapshotStore {
        snapshots: Vec<QuerySnapshot>,
    }

    impl SnapshotStore for MockSnapshotStore {
        fn store_query_snapshot(&mut self, snapshot: QuerySnapshot) {
            self.snapshots.push(snapshot);
        }
    }

    #[test]
    fn test_focus_result_work_items_not_waitable() {
        use atlas_engine::structs::{CoverageTier, Precision, SemanticConfidence};

        // 1. Build a FocusResult with pending_closure_ids and Focus mode.
        let result = atlas_engine::focus::runtime::FocusResult {
            mode: atlas_engine::focus::runtime::IndexMode::Focus,
            precision: Some(Precision {
                coverage: CoverageTier::Partial {
                    gaps: vec![],
                },
                confidence: SemanticConfidence::Medium,
            }),
            gaps: vec![],
            pending_closure_ids: vec!["cl_test_1".to_string(), "cl_test_2".to_string()],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
        };

        // 2. Create a LazyResponse and apply the focus result.
        let lr = LazyResponse::new("test_tool", &serde_json::json!({}));
        let lr = apply_focus_result_to_lr(lr, &result);

        // 3. Build to JSON via a mock store so we can inspect work items.
        let mut mock = MockSnapshotStore {
            snapshots: Vec::new(),
        };
        let (json_str, _is_error) = lr.build(serde_json::json!({"result": "ok"}), &mut mock);
        let resp: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // 4. Assert work items are present and correctly configured.
        let items = resp["work"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);

        for item in items {
            // waitable must be false — closure IDs are NOT task-manager tasks
            assert_eq!(
                item["waitable"].as_bool(),
                Some(false),
                "work item waitable should be false, got: {item}"
            );
            // retry_after_ms must be Some(2000) — poll-friendly interval
            assert_eq!(
                item["retry_after_ms"].as_u64(),
                Some(2000),
                "work item retry_after_ms should be 2000, got: {item}"
            );
            // reason must NOT leak internal "focus" terms
            let reason = item["reason"].as_str().unwrap();
            assert!(
                !reason.starts_with("focus"),
                "reason should not leak internal 'focus' prefix, got: {reason:?}"
            );
            assert_eq!(reason, "background_build");
        }

        // 5. Assert analysis summary doesn't leak "focus" term.
        let summary = resp["analysis"]["summary"].as_str().unwrap();
        assert!(
            !summary.contains("focus analysis"),
            "summary should not contain 'focus analysis', got: {summary:?}"
        );
        assert!(
            summary.contains("scoped analysis"),
            "summary should contain 'scoped analysis', got: {summary:?}"
        );
    }

    // ── Contract-based dispatch tests ────────────────────────────────────

    /// Verify that each tool name routes to the correct contract via `contract_for`.
    #[test]
    fn contract_based_dispatch_routes_to_correct_handler() {
        use crate::tools::tool_contract::{AnalysisNeeds, OverlayKind, QueryNeeds};
        use serde_json::json;

        // Project lifecycle
        assert_eq!(
            contract_for("project", &json!({"action": "open", "project_path": "/tmp"})),
            ToolContract::ProjectLifecycle
        );
        // Status read
        assert_eq!(
            contract_for("project", &json!({"action": "status"})),
            ToolContract::StatusRead
        );
        // Explicit index
        assert_eq!(
            contract_for("index", &json!({"analysis": "structural"})),
            ToolContract::ExplicitIndexBuild
        );
        // Semantic graph queries
        assert_eq!(
            contract_for("calls", &json!({"symbol": "foo"})),
            ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
        );
        assert_eq!(
            contract_for("explore", &json!({"symbol": "foo"})),
            ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
        );
        assert_eq!(
            contract_for("path", &json!({"from": "a", "to": "b"})),
            ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
        );
        assert_eq!(
            contract_for("impact", &json!({"symbol": "foo"})),
            ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
        );
        assert_eq!(
            contract_for("trace", &json!({"kind": "point", "file_path": "x.rs", "line": 1, "column": 1})),
            ToolContract::TraceQuery(QueryNeeds::Full)
        );
        // Store fact queries
        assert_eq!(
            contract_for("symbol", &json!({"symbol": "foo"})),
            ToolContract::StoreFactQuery(QueryNeeds::Manifest)
        );
        assert_eq!(
            contract_for("search", &json!({"query": "foo"})),
            ToolContract::StoreFactQuery(QueryNeeds::Manifest)
        );
        assert_eq!(
            contract_for("file_dependencies", &json!({"file_path": "src/main.rs"})),
            ToolContract::StoreFactQuery(QueryNeeds::Structural)
        );
        // Semantic analysis
        assert_eq!(
            contract_for("branch_diff", &json!({"symbol": "foo"})),
            ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowEffects)
        );
        assert_eq!(
            contract_for("lifecycle", &json!({"symbol": "foo", "field": "ptr"})),
            ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowDomainRules)
        );
        // Overlay mutations
        assert_eq!(
            contract_for("fp_dispatches", &json!({"action": "add", "field_qname": "f", "target_qname": "t"})),
            ToolContract::OverlayMutation(OverlayKind::FunctionPointerDispatch)
        );
        assert_eq!(
            contract_for("fp_dispatches", &json!({"action": "list"})),
            ToolContract::OverlayRead
        );
        assert_eq!(
            contract_for("domain_rules", &json!({"action": "add", "rule_kind": "free_fn", "pattern": "xfree"})),
            ToolContract::OverlayMutation(OverlayKind::DomainRules)
        );
        assert_eq!(
            contract_for("domain_rules", &json!({"action": "list"})),
            ToolContract::OverlayRead
        );
        // Task control
        assert_eq!(
            contract_for("tasks", &json!({})),
            ToolContract::TaskControl
        );
        assert_eq!(
            contract_for("task_status", &json!({"task_id": "abc"})),
            ToolContract::TaskControl
        );
        assert_eq!(
            contract_for("wait_for_task", &json!({"task_id": "abc"})),
            ToolContract::TaskControl
        );
        assert_eq!(
            contract_for("resume_task", &json!({"query_id": "abc"})),
            ToolContract::TaskControl
        );
    }

    /// Calling an unknown tool name must return is_error=true.
    #[test]
    fn unknown_tool_returns_error() {
        use serde_json::json;

        let store = test_store();
        let tmp = std::env::temp_dir();
        let mut router = ToolRouter::new_empty(store, tmp);
        let ctx = ToolCallContext::empty();
        let result = router.call_tool(&ctx, "nonexistent_tool", &json!({}));
        assert!(result.is_error.unwrap_or(false), "unknown tool should set is_error=true");
    }

    /// Every tool registered via `make_all_tools()` must have a valid dispatch path
    /// through `contract_for()` and the corresponding sub-dispatcher.
    #[test]
    fn contract_dispatch_handles_all_registered_tools() {
        use serde_json::json;

        let all_tools = make_all_tools();
        for tool in &all_tools {
            let name = &tool.name;
            let contract = contract_for(name, &json!({}));

            // Verify the contract routes to a sub-dispatcher that handles this name.
            let handled = tool_has_dispatch_path(name, &contract);
            assert!(
                handled,
                "tool '{name}' maps to contract {contract:?} but sub-dispatcher does not handle it"
            );
        }
    }

    /// Returns true if the given tool name has a matching arm in the sub-dispatcher
    /// for its contract type.
    fn tool_has_dispatch_path(name: &str, contract: &ToolContract) -> bool {
        match contract {
            // ProjectLifecycle → handled by handle_project directly
            ToolContract::ProjectLifecycle => true,
            // StatusRead → dispatch_status_read only handles "project"
            ToolContract::StatusRead => name == "project",
            // ExplicitIndexBuild → handle_index, any name works (only "index" routes here)
            ToolContract::ExplicitIndexBuild => name == "index",
            // SemanticGraphQuery → dispatch_graph_query
            ToolContract::SemanticGraphQuery(_) => {
                matches!(name, "calls" | "explore" | "path" | "impact" | "symbol")
            }
            // TraceQuery → dispatch_trace_query
            ToolContract::TraceQuery(_) => {
                matches!(name, "trace")
            }
            // StoreFactQuery → dispatch_store_query
            ToolContract::StoreFactQuery(_) => {
                matches!(name, "symbol" | "search" | "file_dependencies")
            }
            // SemanticAnalysis → dispatch_analysis
            ToolContract::SemanticAnalysis(_) => {
                matches!(name, "branch_diff" | "lifecycle")
            }
            // OverlayMutation / OverlayRead → dispatch_overlay
            ToolContract::OverlayMutation(_) | ToolContract::OverlayRead => {
                matches!(name, "fp_dispatches" | "domain_rules")
            }
            // TaskControl → dispatch_task_control
            ToolContract::TaskControl => {
                matches!(name, "tasks" | "task_status" | "wait_for_task" | "resume_task")
            }
        }
    }

    // ── E2E contract dispatch tests ────────────────────────────────────────
    //
    // These tests validate the full call_tool() → contract_for() → handler
    // dispatch chain for every ToolContract variant.  They verify:
    //   - The tool name routes to the correct contract
    //   - The contract routes to the correct handler (no "Unknown tool" error)
    //   - is_error field reflects expected behavior
    //
    // Handler logic is NOT tested here — each handler has its own unit tests.

    /// Extract text from the first content block of a CallToolResult.
    fn extract_text(result: &CallToolResult) -> String {
        match &result.content[0] {
            ContentBlock::Text { text } => text.clone(),
        }
    }

    // ── Test 1: ProjectLifecycle contract — "project" with action="open" ─

    #[test]
    fn e2e_project_lifecycle_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();
        // action=open is ProjectLifecycle; missing project_path will error
        // but the contract routing itself should work.
        let args = serde_json::json!({"action": "open"});
        let result = router.call_tool(&ctx, "project", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "project open should route to ProjectLifecycle, got: {text}"
        );
    }

    // ── Test 2: StatusRead contract — "project" with action="status"/"files" ─

    #[test]
    fn e2e_status_read_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();

        // action=status → StatusRead
        let args = serde_json::json!({"action": "status"});
        let result = router.call_tool(&ctx, "project", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "project status should route to StatusRead, got: {text}"
        );
        assert_eq!(result.is_error, Some(false), "status should succeed");

        // action=files → StatusRead
        let args2 = serde_json::json!({"action": "files"});
        let result2 = router.call_tool(&ctx, "project", &args2);
        let text2 = extract_text(&result2);
        assert!(
            !text2.contains("Unknown tool"),
            "project files should route to StatusRead, got: {text2}"
        );
        assert_eq!(result2.is_error, Some(false), "files should succeed");
    }

    // ── Test 3: ExplicitIndexBuild contract — "index" tool ───────────────

    #[test]
    fn e2e_index_build_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({"analysis": "manifest"});
        let result = router.call_tool(&ctx, "index", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "index should route to ExplicitIndexBuild, got: {text}"
        );
    }

    // ── Test 4: SemanticGraphQuery contract — "calls" / "explore" ────────

    #[test]
    fn e2e_graph_query_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        // Graph must be initialized before SemanticGraphQuery tools can query.
        let _ = router.ensure_graph_initialized();
        let ctx = ToolCallContext::empty();

        // calls → SemanticGraphQuery(CallGraph)
        let args = serde_json::json!({"symbol": "test_func"});
        let result = router.call_tool(&ctx, "calls", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "calls should route to SemanticGraphQuery, got: {text}"
        );

        // explore → SemanticGraphQuery(CallGraph)
        let args2 = serde_json::json!({"symbol": "test_func"});
        let result2 = router.call_tool(&ctx, "explore", &args2);
        let text2 = extract_text(&result2);
        assert!(
            !text2.contains("Unknown tool"),
            "explore should route to SemanticGraphQuery, got: {text2}"
        );
    }

    // ── Test 5: StoreFactQuery contract — "search" / "symbol" ────────────

    #[test]
    fn e2e_store_query_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let _ = router.ensure_graph_initialized(); // "symbol" requires graph
        let ctx = ToolCallContext::empty();

        // search → StoreFactQuery(Manifest)
        let args = serde_json::json!({"query": "test"});
        let result = router.call_tool(&ctx, "search", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "search should route to StoreFactQuery, got: {text}"
        );

        // symbol with default view → StoreFactQuery(Manifest)
        let args2 = serde_json::json!({"symbol": "test"});
        let result2 = router.call_tool(&ctx, "symbol", &args2);
        let text2 = extract_text(&result2);
        assert!(
            !text2.contains("Unknown tool"),
            "symbol should route to StoreFactQuery, got: {text2}"
        );
    }

    // ── Test 6: SemanticAnalysis contract — "lifecycle" / "branch_diff" ──

    #[test]
    fn e2e_analysis_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();

        // lifecycle → SemanticAnalysis(CfgDataflowDomainRules)
        let args = serde_json::json!({"symbol": "test_func", "field": "data"});
        let result = router.call_tool(&ctx, "lifecycle", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "lifecycle should route to SemanticAnalysis, got: {text}"
        );

        // branch_diff → SemanticAnalysis(CfgDataflowEffects)
        let args2 = serde_json::json!({"symbol": "test_func"});
        let result2 = router.call_tool(&ctx, "branch_diff", &args2);
        let text2 = extract_text(&result2);
        assert!(
            !text2.contains("Unknown tool"),
            "branch_diff should route to SemanticAnalysis, got: {text2}"
        );
    }

    // ── Test 7: OverlayMutation / OverlayRead contract ────────────────────

    #[test]
    fn e2e_overlay_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();

        // domain_rules list → OverlayRead
        let args = serde_json::json!({"action": "list"});
        let result = router.call_tool(&ctx, "domain_rules", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "domain_rules list should route to OverlayRead, got: {text}"
        );

        // fp_dispatches list → OverlayRead
        let args2 = serde_json::json!({"action": "list"});
        let result2 = router.call_tool(&ctx, "fp_dispatches", &args2);
        let text2 = extract_text(&result2);
        assert!(
            !text2.contains("Unknown tool"),
            "fp_dispatches list should route to OverlayRead, got: {text2}"
        );

        // domain_rules add → OverlayMutation(DomainRules) — needs args, will
        // fail validation but routing should be correct.
        let args3 = serde_json::json!({"action": "add", "rule_kind": "free_fn", "pattern": "xfree"});
        let result3 = router.call_tool(&ctx, "domain_rules", &args3);
        let text3 = extract_text(&result3);
        assert!(
            !text3.contains("Unknown tool"),
            "domain_rules add should route to OverlayMutation, got: {text3}"
        );
    }

    // ── Test 8: TaskControl contract — "tasks" / "task_status" ───────────

    #[test]
    fn e2e_task_control_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();

        // tasks → TaskControl
        let args = serde_json::json!({});
        let result = router.call_tool(&ctx, "tasks", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "tasks should route to TaskControl, got: {text}"
        );

        // task_status → TaskControl
        let args2 = serde_json::json!({"task_id": "nonexistent"});
        let result2 = router.call_tool(&ctx, "task_status", &args2);
        let text2 = extract_text(&result2);
        assert!(
            !text2.contains("Unknown tool"),
            "task_status should route to TaskControl, got: {text2}"
        );

        // wait_for_task → TaskControl
        let args3 = serde_json::json!({"task_id": "nonexistent"});
        let result3 = router.call_tool(&ctx, "wait_for_task", &args3);
        let text3 = extract_text(&result3);
        assert!(
            !text3.contains("Unknown tool"),
            "wait_for_task should route to TaskControl, got: {text3}"
        );

        // resume_task → TaskControl
        let args4 = serde_json::json!({"query_id": "nonexistent"});
        let result4 = router.call_tool(&ctx, "resume_task", &args4);
        let text4 = extract_text(&result4);
        assert!(
            !text4.contains("Unknown tool"),
            "resume_task should route to TaskControl, got: {text4}"
        );
    }

    // ── Test 9: TraceQuery contract — "trace" tool (SemanticGraphQuery) ──

    #[test]
    fn e2e_trace_query_contract_routes_correctly() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let _ = router.ensure_graph_initialized();
        let ctx = ToolCallContext::empty();

        // trace kind=callers → SemanticGraphQuery(Full) → dispatch_graph_query
        let args = serde_json::json!({"kind": "callers", "symbol": "test_func"});
        let result = router.call_tool(&ctx, "trace", &args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "trace should route via SemanticGraphQuery to graph handler, got: {text}"
        );
    }

    // ── Test 10: ctx forwarding — tools accept ToolCallContext::empty() ──

    #[test]
    fn e2e_ctx_forwarding_does_not_panic() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let _ = router.ensure_graph_initialized();
        let ctx = ToolCallContext::empty();

        // Tools that receive ctx: search, symbol, index, trace
        let cases: &[(&str, serde_json::Value)] = &[
            ("search", serde_json::json!({"query": "test"})),
            ("symbol", serde_json::json!({"symbol": "test"})),
            ("index", serde_json::json!({})),
            ("trace", serde_json::json!({"kind": "callers", "symbol": "test"})),
        ];

        for (tool_name, args) in cases {
            let result = router.call_tool(&ctx, tool_name, args);
            let text = extract_text(&result);
            assert!(
                !text.contains("Unknown tool"),
                "tool '{tool_name}' should accept empty ctx without panic, got: {text}"
            );
        }
    }

    // ── Bonus: non-existent tool returns StatusRead fallback ──────────────

    #[test]
    fn e2e_unknown_tool_falls_back_to_status_read() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({});
        let result = router.call_tool(&ctx, "nonexistent_tool", &args);
        let text = extract_text(&result);
        assert!(
            text.contains("Unknown tool"),
            "Unknown tool should return error via StatusRead fallback, got: {text}"
        );
        assert_eq!(result.is_error, Some(true), "unknown tool should be an error");
    }

    // ── Phase 9: auto-inject graph precision for SemanticGraphQuery tools ─

    #[test]
    fn auto_injects_graph_precision_for_semantic_graph_query() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol(&store, file_id, "test_func");
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let _ = router.ensure_graph_initialized();
        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({"symbol": "test_func.test_func"});
        let result = router.call_tool(&ctx, "calls", &args);
        let text = extract_text(&result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        // Precision should be present: either graph precision (focus_partial)
        // or per-query focus precision from lazy extraction. Both are valid.
        assert!(
            val.get("precision").is_some(),
            "should inject graph precision for graph tools, got: {text}"
        );
    }

    #[test]
    fn does_not_inject_precision_for_non_graph_tools() {
        let store = test_store();
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let _ = router.ensure_graph_initialized();
        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({"action": "list"});
        let result = router.call_tool(&ctx, "domain_rules", &args);
        let text = extract_text(&result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            val.get("precision").is_none(),
            "should NOT inject precision for non-graph tools"
        );
    }

    #[test]
    fn does_not_inject_precision_when_full_canonical() {
        let store = test_store();
        let file_id = register_test_file(&store, "test.ts");
        insert_test_symbol(&store, file_id, "test_func");
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let _ = router.ensure_graph_initialized();
        // Override mode to FullCanonical — graph precision injection should skip.
        router.active.graph_runtime.mode = GraphMode::FullCanonical;
        let ctx = ToolCallContext::empty();
        let args = serde_json::json!({"symbol": "test_func.test_func"});
        let result = router.call_tool(&ctx, "calls", &args);
        let text = extract_text(&result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        // Graph precision (mode=focus_partial) must NOT be present.
        // Focus precision (coverage/confidence) from lazy extraction may still
        // be present — that's a separate concern.
        if let Some(prec) = val.get("precision") {
            assert!(
                prec.get("mode").is_none() || prec["mode"] != "focus_partial",
                "should NOT inject graph precision for FullCanonical, got: {text}"
            );
        }
    }


}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;
