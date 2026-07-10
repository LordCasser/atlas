//! MCP tool definitions and dispatch.
//!
//! Each tool has: name, description, inputSchema, handler.
//! The ToolRouter maps tool names to handlers and produces the tools/list response.
//!
//! Handler methods are organized by capability category in sub-modules:
//!   status, search, graph, context, trace, lifecycle, branch_diff.

use atlas_engine::CandidateProvider;
use atlas_engine::ContextBuilder;
use atlas_engine::DefaultCandidateProvider;
use atlas_engine::FileId;
use atlas_engine::SearchEngine;
use atlas_engine::Store;
use atlas_engine::SymbolId;
use atlas_engine::TraceDiagnostic;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use super::protocol::{CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema};

use serde_json::{Value, json};
use std::sync::atomic::Ordering;

use crate::tools::analysis_envelope::{AnalysisEnvelope, SnapshotStore};
use crate::tools::query_snapshot::QuerySnapshot;
#[cfg(test)]
use crate::tools::runtime::graph_runtime::EdgeProvenance;
use symbol_selector::{ScoredCandidate, SymbolInput, parse_symbol_input};

use crate::tools::active_project::ActiveProject;
use crate::tools::project_slot::ProjectSlot;
use crate::tools::tool_contract::{ToolContract, contract_for};

/// Progress report tuple: (progress, total, message)
pub(crate) type ProgressReport = (f64, Option<f64>, Option<String>);
/// Channel sender for progress updates during long-running operations.
pub(crate) type ProgressSender = tokio::sync::mpsc::UnboundedSender<ProgressReport>;

/// Maximum number of ambiguous candidates to display in diagnostics.
/// Beyond this, candidates are truncated to avoid log flooding in large projects.
pub(crate) const MAX_AMBIGUOUS_CANDIDATES: usize = 5;
const RETRYABLE_CANDIDATE_LIMIT: usize = 8;

// -------------------------------------------------------------------
// ToolCallContext — request-scoped progress capabilities
// -------------------------------------------------------------------

/// Request-scoped context for a single tool call.
///
/// Carries the progress sender so handlers do not rely on global mutable state
/// on [`ToolRouter`].
#[derive(Clone)]
pub struct ToolCallContext {
    /// MCP progress notification sender (None = no progress token).
    pub progress_sender: Option<ProgressSender>,
}

impl ToolCallContext {
    /// Create a context with no progress capabilities.
    pub fn empty() -> Self {
        Self {
            progress_sender: None,
        }
    }

    /// Create a context from a progress sender.
    pub fn with_progress_sender(sender: ProgressSender) -> Self {
        Self {
            progress_sender: Some(sender),
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
pub(crate) mod analysis_envelope;
pub(crate) mod annotations;
pub(crate) mod atlas_jobs;
pub(crate) mod branch_diff;
pub(crate) mod context;
pub(crate) mod dependencies;
pub(crate) mod dependents;
pub(crate) mod domain_rules;
pub(crate) mod graph;
pub(crate) mod lazy_refresh;
pub(crate) mod lifecycle;
pub(crate) mod open_project;
pub(crate) mod project_slot;
pub(crate) mod query_snapshot;
pub(crate) mod resume;
pub(crate) mod runtime;
pub(crate) mod search;
pub(crate) mod status;
pub(crate) mod symbol_selector;
pub(crate) mod tool_contract;
#[cfg(test)]
mod handler_purity;
pub(crate) mod trace;
pub(crate) mod usages;

/// Apply focus-aware envelope fields from a [`FocusResult`] to the AnalysisEnvelope.
///
/// Takes `&FocusResult` directly and merges precision, coverage, gaps,
/// and retry guidance into the builder.
pub(crate) fn apply_focus_result_to_lr(
    lr: analysis_envelope::AnalysisEnvelope,
    result: &atlas_engine::focus::runtime::FocusResult,
) -> analysis_envelope::AnalysisEnvelope {
    let mut lr = lr;

    if result.access != atlas_engine::focus::runtime::AccessStrategy::Focus {
        return lr;
    }

    lr = lr.with_focus_result(result.clone());

    // Always inject coverage distribution from FocusResult
    if let Some(ref counts) = result.coverage_counts {
        lr = lr.with_coverage_counts(counts.clone());
    }

    let (pending_count, retry_after_ms) = result.pending_work_count_and_eta_ms();

    if pending_count == 0 {
        // Terminal: result is ready to use
        let mut lr = lr
            .with_analysis_scope("local".to_string())
            .with_analysis_summary(
                "Focus analysis complete: all background jobs have finished.".to_string(),
            )
            .with_analysis_basis(vec!["manifest".into(), "structural".into()]);
        let mut gaps: Vec<_> = result.gaps.iter().map(known_gap_record).collect();
        if let Some(tracker) = &result.job_tracker {
            gaps.extend(
                tracker
                    .failures_for(&result.pending_closure_ids)
                    .into_iter()
                    .map(|(_, reason)| analysis_envelope::GapRecord {
                        scope: "focus_closure".to_string(),
                        reason: "background_refinement_failed".to_string(),
                        detail: reason,
                    }),
            );
        }
        if !gaps.is_empty() {
            lr = lr.with_gap_records(gaps);
        }
        lr
    } else {
        // Non-terminal: tracked background closures or raw extraction jobs are still running.
        lr = lr
            .with_analysis_scope("local".to_string())
            .with_analysis_summary(format!(
                "Focus analysis still expanding: {pending_count} pending job(s) remaining.",
            ))
            .with_analysis_basis(vec!["manifest".into(), "structural".into()]);
        lr.with_analysis_retry_after_ms(retry_after_ms)
    }
}

fn known_gap_record(gap: &atlas_engine::structs::KnownGap) -> analysis_envelope::GapRecord {
    use atlas_engine::structs::KnownGap;

    let (scope, reason, detail) = match gap {
        KnownGap::ExtractionFailed { file, reason } => (
            file.clone(),
            "extraction_failed",
            format!("Structural extraction failed: {reason}"),
        ),
        KnownGap::UnresolvedImport { from, import_path } => (
            from.clone(),
            "unresolved_import",
            format!("Import '{import_path}' could not be resolved from this file."),
        ),
        KnownGap::IndirectCall { callsite, reason } => (
            callsite.clone(),
            "indirect_call",
            format!("Indirect call target is unresolved: {reason}"),
        ),
        KnownGap::TypeOutside { type_name, ref_by } => (
            ref_by.clone(),
            "type_outside_closure",
            format!("Referenced type '{type_name}' is outside the analyzed closure."),
        ),
        KnownGap::BudgetExhausted {
            strategy,
            remaining,
        } => (
            "focus_closure".to_string(),
            "budget_exhausted",
            format!("Strategy '{strategy}' stopped with {remaining} item(s) remaining."),
        ),
        KnownGap::ConditionalBranch {
            symbol,
            guard,
            branches,
        } => (
            symbol.clone(),
            "conditional_branch",
            format!("Guard '{guard}' has {branches} branch(es) not fully covered."),
        ),
        KnownGap::CodeGenerationNotExpanded { at, generator } => (
            at.clone(),
            "code_generation_not_expanded",
            format!("Generated code from '{generator}' was not expanded."),
        ),
        KnownGap::HighFanoutName {
            name, candidates, ..
        } => (
            name.clone(),
            "high_fanout_name",
            format!("Name resolution has {candidates} candidates."),
        ),
        KnownGap::SymbolHintsIncomplete { name, coverage_pct } => (
            name.clone(),
            "symbol_hints_incomplete",
            format!("Symbol hint coverage is {coverage_pct}%."),
        ),
        KnownGap::VisibilityHidden { symbol, reason } => (
            symbol.clone(),
            "visibility_hidden",
            format!("Symbol is hidden from this closure: {reason}"),
        ),
    };

    analysis_envelope::GapRecord {
        scope,
        reason: reason.to_string(),
        detail,
    }
}

// -------------------------------------------------------------------
// ToolRouter
// -------------------------------------------------------------------

/// Dispatches tools/list and tools/call.
pub struct ToolRouter {
    pub(crate) project: ProjectSlot,
    tools: Vec<Tool>,
    replay_focus_result: Option<atlas_engine::focus::runtime::FocusResult>,
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
        let active =
            ActiveProject::new(store, project_root).expect("Failed to construct ActiveProject");
        // Initialize graph state with pre-built search and context engines.
        active.graph_runtime.state.init_with(search, context);
        Self {
            project: ProjectSlot::new(Some(active)), // already Arc<ActiveProject>
            tools: make_all_tools(),
            replay_focus_result: None,
        }
    }

    /// Create a router without building the graph (fast startup).
    /// Graph is built lazily on the first request via `ensure_graph_initialized`.
    pub fn new_empty(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        let active =
            ActiveProject::new(store, project_root).expect("Failed to construct ActiveProject");
        Self {
            project: ProjectSlot::new(Some(active)), // already Arc<ActiveProject>
            tools: make_all_tools(),
            replay_focus_result: None,
        }
    }

    /// Create a router with no active project.
    ///
    /// This is the normal stdio MCP startup path. Clients must call
    /// `project(action="open")` before using project-scoped tools.
    pub fn new_unopened() -> Self {
        Self {
            project: ProjectSlot::new(None),
            tools: make_all_tools(),
            replay_focus_result: None,
        }
    }

    fn for_resume(
        project: Arc<ActiveProject>,
        focus_result: Option<atlas_engine::focus::runtime::FocusResult>,
    ) -> Self {
        Self {
            project: ProjectSlot::new(Some(project)),
            tools: Vec::new(),
            replay_focus_result: focus_result,
        }
    }

    /// Access the active project as `Arc<ActiveProject>`. Panics if no project is active.
    /// Callers are protected by the gate in `call_tool()`.
    fn project(&self) -> Arc<ActiveProject> {
        self.project
            .get()
            .expect("call_tool gate ensures project is active")
    }

    /// Return the backing store.
    pub fn store(&self) -> Arc<Store> {
        self.project().store.clone()
    }

    fn active_extraction_job_count(&self, job_ids: &[String]) -> usize {
        job_ids
            .iter()
            .filter(|job_id| {
                self.project()
                    .store
                    .get_extraction_job(job_id)
                    .ok()
                    .flatten()
                    .is_some_and(|job| matches!(job.status.as_str(), "queued" | "building"))
            })
            .count()
    }

    fn focus_pending_count_and_eta_ms(
        &self,
        result: &atlas_engine::focus::runtime::FocusResult,
    ) -> (usize, u64) {
        let (closure_pending, closure_eta) = result
            .job_tracker
            .as_ref()
            .map(|tracker| tracker.pending_count_and_eta_ms(&result.pending_closure_ids))
            .unwrap_or((0, 0));
        let extraction_pending =
            self.active_extraction_job_count(&result.pending_extraction_job_ids);
        let pending = closure_pending + extraction_pending;
        if pending == 0 {
            return (0, 0);
        }
        let extraction_eta = 5000 * extraction_pending as u64;
        (pending, (closure_eta + extraction_eta).clamp(5000, 60000))
    }

    /// Return whether a tool needs the in-memory graph/search/context snapshot.
    ///
    /// Store-backed tools intentionally do not force graph construction. This
    /// keeps MCP `initialize`, `tools/list`, status, files, trace, usages,
    /// dependencies, dependents and capabilities responsive on large projects.
    pub fn tool_requires_graph(name: &str) -> bool {
        matches!(name, "symbol" | "calls" | "path" | "explore" | "impact")
    }

    /// Build the graph engine on first use.
    /// This is called only for graph-backed tool calls after the MCP handshake
    /// completes, so the client doesn't timeout waiting for a startup response.
    pub fn ensure_graph_initialized(&self) -> anyhow::Result<()> {
        self.project().graph_runtime.ensure_initialized()?;
        Ok(())
    }

    /// Unified focus query preparation for focus-driven lazy analysis.
    ///
    /// Returns `(Some(FocusResult), warnings)` when focus analysis completed,
    /// or `(None, warnings)` when focus is not needed or unavailable.
    ///
    /// Equivalent to [`prepare_focus_query_with_roots`] with an empty
    /// `include_roots` vector. Tools that accept `include_roots` from their MCP
    /// arguments should call [`prepare_focus_query_with_roots`] instead so that
    /// angle-bracket `#include <...>` directives resolve to project headers.
    pub fn prepare_focus_query(
        &self,
        intent: Option<atlas_engine::QueryIntent>,
    ) -> (
        Option<atlas_engine::focus::runtime::FocusResult>,
        Vec<String>,
    ) {
        self.prepare_focus_query_with_roots(intent, Vec::new())
    }

    /// Unified focus query preparation carrying request-scoped `include_roots`.
    ///
    /// This is the entry point that wires MCP `include_roots` into the focus
    /// windows for this query, so foreground and background closure builds both
    /// resolve angle-bracket `#include <...>` directives against the request's
    /// project headers. Roots are carried by value on the query windows; they
    /// are never persisted or stored on the cached closure engine.
    pub fn prepare_focus_query_with_roots(
        &self,
        intent: Option<atlas_engine::QueryIntent>,
        include_roots: Vec<atlas_engine::IncludeRoot>,
    ) -> (
        Option<atlas_engine::focus::runtime::FocusResult>,
        Vec<String>,
    ) {
        if let Some(result) = self.replay_focus_result.as_ref() {
            if result.pending_extraction_job_ids.is_empty()
                || self.active_extraction_job_count(&result.pending_extraction_job_ids) > 0
            {
                return (Some(result.clone()), vec![]);
            }
        }

        let project = self.project();
        // 1. Full index already exists — no focus needed.
        if project.query_runtime.has_full_index(&project.store) {
            return (None, vec![]);
        }

        // 2. No intent — nothing to prepare.
        let intent = match intent {
            Some(i) => i,
            None => return (None, vec![]),
        };

        // 3. Delegate FocusRuntime interaction to QueryRuntime.
        let (focus_result, warnings) =
            project
                .query_runtime
                .prepare(&intent, &project.store, include_roots);

        // 4. Post-processing: record lazy writes and refresh graph.
        if let Some(ref result) = focus_result {
            let materialized_files = result.materialized_files();
            if !materialized_files.is_empty() {
                project
                    .query_runtime
                    .lazy_refresh_queue
                    .record_lazy_writes(&materialized_files);
            }
            if let Err(e) = self.maybe_refresh_graph() {
                let mut combined = warnings.clone();
                combined.push(format!("Focus succeeded but graph refresh failed: {e}"));
                return (Some(result.clone()), combined);
            }
        }

        (focus_result, warnings)
    }

    /// Queue background focus warming for already-identified hot files.
    ///
    /// This is the non-blocking companion to [`prepare_focus_query`]. It is
    /// used by latency-sensitive tools that have already returned a bounded
    /// result and want later calls/resume_query to see richer local facts.
    pub(crate) fn enqueue_background_file_focus(&self, file_ids: &[FileId]) -> Vec<String> {
        if file_ids.is_empty() {
            return Vec::new();
        }

        let project = self.project();
        match project.query_runtime.enqueue_file_focus_warm(file_ids) {
            Ok(job_ids) => job_ids,
            Err(err) => {
                tracing::warn!("background focus warming enqueue failed: {err:#}");
                Vec::new()
            }
        }
    }

    /// Find likely source files for an unresolved symbol without blocking on
    /// project-wide indexing. Candidate discovery lives in atlas-engine so MCP
    /// stays a thin response-contract layer.
    pub(crate) fn candidate_file_ids_for_symbol(&self, symbol: &str) -> Vec<FileId> {
        let project = self.project();
        let provider =
            DefaultCandidateProvider::new(project.store.clone(), Some(project.root.clone()));
        let mut seen = HashSet::new();
        provider
            .candidates_for_symbol(symbol)
            .unwrap_or_default()
            .into_iter()
            .filter(|file_id| seen.insert(*file_id))
            .take(RETRYABLE_CANDIDATE_LIMIT)
            .collect()
    }

    pub(crate) fn candidate_file_paths(&self, file_ids: &[FileId]) -> Vec<String> {
        let project = self.project();
        let mut seen = HashSet::new();
        file_ids
            .iter()
            .filter_map(|file_id| {
                if !seen.insert(*file_id) {
                    return None;
                }
                Some(
                    project
                        .store
                        .find_file_inventory_by_id(file_id)
                        .ok()
                        .flatten()
                        .map(|row| row.path)
                        .unwrap_or_else(|| project.store_query_runtime.resolve_file_path(file_id)),
                )
            })
            .take(RETRYABLE_CANDIDATE_LIMIT)
            .collect()
    }

    /// Build a bounded, retryable response for cold symbol lookups.
    ///
    /// This is used instead of returning a plain "Symbol not found" error when
    /// a focus-mode project has not materialized the relevant local facts yet.
    /// It gives MCP clients a concrete wait/resume contract and still warms
    /// likely candidate files in the background.
    pub(crate) fn retryable_symbol_not_found_response(
        &self,
        tool_name: &'static str,
        args: &serde_json::Value,
        symbol: &str,
        suggestions: Vec<String>,
        detail: Option<String>,
    ) -> (String, bool) {
        let file_ids = self.candidate_file_ids_for_symbol(symbol);
        let candidate_files = self.candidate_file_paths(&file_ids);
        let background_jobs = self.enqueue_background_file_focus(&file_ids);

        let message = if background_jobs.is_empty() {
            "The symbol is not available in the current local focus closure yet. Retry this request after the suggested delay, or pass a SymbolSelector with file_path/scope to constrain the local region."
        } else {
            "The symbol is not available in the current local focus closure yet. Background scoped analysis has been started; retry this request after the suggested delay, or pass a SymbolSelector with file_path/scope to constrain the local region."
        };

        let mut resp = json!({
            "symbol": symbol,
            "status": "unresolved",
            "message": message,
            "candidate_files": candidate_files,
        });
        if !suggestions.is_empty() {
            resp["suggestions"] = json!(suggestions);
        }
        if let Some(detail) = detail {
            resp["detail"] = json!(detail);
        }

        AnalysisEnvelope::new(tool_name, args)
            .with_is_error(false)
            .with_analysis_scope("local".into())
            .with_analysis_summary(
                "bounded unresolved result; background scoped analysis is preparing local symbol facts"
                    .into(),
            )
            .with_analysis_basis(vec!["manifest".into(), "structural".into()])
            .with_analysis_retry_after_ms(8000)
            .build(resp, self)
    }

    /// Rebuild the graph snapshot from the store if the index signature changed.
    fn rebuild_if_signature_changed(&self, reason: &str) -> anyhow::Result<()> {
        let project = self.project();
        let current = project.store.index_signature().unwrap_or_else(|_| {
            project
                .query_runtime
                .cache
                .cached_signature
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
        if current
            != *project
                .graph_runtime
                .state
                .last_graph_signature
                .lock()
                .unwrap()
        {
            tracing::info!("{reason}");
            let graph = Arc::new(atlas_engine::GraphEngine::from_store(&project.store, 0.3)?);
            project
                .graph_runtime
                .state
                .swap_graph(&project.store, graph);
            // Re-check whether a manual full index now exists (layer distribution
            // may have changed after external index/sync or lazy structural).
            *project
                .query_runtime
                .cache
                .cached_manual_full_index
                .write()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
        *project.query_runtime.cache.cached_signature.lock().unwrap() = current;
        Ok(())
    }

    /// Switch the active project to a new store+root, clearing graph/cache state.
    ///
    /// This is the core mechanism for `atlas_open_project` and project switching.
    /// After activation, the next graph-backed tool call will lazily rebuild the
    /// snapshot from the new store.
    pub(crate) fn activate_project(&self, project_root: std::path::PathBuf, store: Arc<Store>) {
        // ActiveProject::new injects FocusMaterialize into FocusRuntime at construction.
        self.project.replace(
            ActiveProject::new(store, project_root)
                .expect("Failed to construct ActiveProject during project activation"),
        );
    }

    /// Ensure the in-memory call-graph reflects any newly extracted structural data.
    ///
    /// **Refresh responsibility**: this method is called internally by
    /// `prepare_focus_query` whenever new files were built. Callers that
    /// use that helper do **not** need to call `maybe_refresh_graph` separately.
    ///
    /// Callers that modify the store independently (e.g. through a full re-index
    /// signal) may still need to call this to pick up changes.
    pub fn maybe_refresh_graph(&self) -> anyhow::Result<()> {
        let project = self.project();
        if !project
            .graph_runtime
            .state
            .graph_initialized
            .load(Ordering::Acquire)
        {
            return Ok(());
        }

        // Drain the project-wide background refresh feed before taking the
        // incremental batch. This is independent of replay state, so a fresh
        // graph request sees writes completed by an earlier closure or warming
        // job without carrying closure IDs across requests.
        project.query_runtime.record_background_built_files();

        // Step 1: Always flush pending incremental writes (no cooldown).
        // This ensures lazy writes from THIS request are visible before graph queries.
        let batch = project
            .query_runtime
            .lazy_refresh_queue
            .take_incremental_batch(500);
        if let Err(error) = project
            .graph_runtime
            .state
            .refresh_graph_for_files(&project.store, &batch)
        {
            project
                .query_runtime
                .lazy_refresh_queue
                .requeue_incremental_batch(&batch);
            return Err(error);
        }
        // Cache invalidation: new store data may have changed layer distribution.
        // Lazy writes affect the graph — bump generation so the next check triggers rebuild.
        if !batch.is_empty() {
            *project
                .query_runtime
                .cache
                .cached_manual_full_index
                .write()
                .unwrap_or_else(|e| e.into_inner()) = None;
            project
                .graph_runtime
                .invalidation
                .graph_generation
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // The incremental per-file refresh above has already brought the
            // in-memory graph up to date for these lazy writes. Mark this
            // generation as consumed so the stale check below does not perform
            // an immediate synchronous full-graph rebuild on large projects.
            project.graph_runtime.mark_graph_fresh();
        }

        // Step 2: Deferred full rebuild — try to apply a background-built graph,
        // or spawn the rebuild thread. NEVER blocks the current request.
        project.graph_runtime.state.try_apply_or_spawn_rebuild(
            Arc::clone(&project.store),
            Arc::clone(&project.query_runtime.lazy_refresh_queue),
        );

        // Step 3: Generation-based staleness check.
        // Replaces the old signature+TTL check. If graph_generation has been bumped
        // since the last refresh (e.g. by overlay mutations or lazy writes), trigger
        // a full rebuild unconditionally without checking the store signature.
        if project.graph_runtime.is_graph_stale() {
            tracing::info!("Graph generation changed, triggering full rebuild");
            let graph = Arc::new(atlas_engine::GraphEngine::from_store(&project.store, 0.3)?);
            project
                .graph_runtime
                .state
                .swap_graph(&project.store, graph);
            let current = project.store.index_signature().unwrap_or_else(|_| {
                project
                    .query_runtime
                    .cache
                    .cached_signature
                    .lock()
                    .unwrap()
                    .clone()
            });
            *project.query_runtime.cache.cached_signature.lock().unwrap() = current;
            *project
                .query_runtime
                .cache
                .cached_manual_full_index
                .write()
                .unwrap_or_else(|e| e.into_inner()) = None;
            project.graph_runtime.mark_graph_fresh();
        }

        Ok(())
    }

    /// Force-refresh the graph snapshot regardless of cache cooldown.
    ///
    /// Called after lazy structural extraction writes new facts to the DB
    /// (via the context tool's tier-3 symbol resolution), so that the
    /// in-memory graph includes the newly parsed edges before graph-backed
    /// tools run their queries.
    pub(crate) fn force_refresh_graph(&self) -> anyhow::Result<()> {
        let project = self.project();
        if !project
            .graph_runtime
            .state
            .graph_initialized
            .load(Ordering::Acquire)
        {
            return Ok(());
        }
        *project
            .query_runtime
            .cache
            .last_signature_check
            .lock()
            .unwrap() = std::time::Instant::now();
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
        &self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: &Value,
    ) -> CallToolResult {
        // Each handler returns (result_text, is_error).
        // is_error=true only for genuine failures (lookup errors, I/O errors, unknown tool).
        let contract = contract_for(name, arguments);

        // Gate: only project lifecycle and project status tools are allowed
        // without an active project. All other tools require an active project.
        match &contract {
            ToolContract::ProjectLifecycle => {}
            ToolContract::StatusRead if name == "project" => {}
            _ => {
                if let Err(msg) = self.project.get().map(|_| ()) {
                    return CallToolResult {
                        content: vec![ContentBlock::text(msg)],
                        is_error: Some(true),
                    };
                }
            }
        }

        // Phase 7a: Resource preparation based on contract.
        //
        // Graph-backed tools need the graph snapshot initialized and
        // refreshed before dispatch.  Doing this inside call_tool() means the
        // contract itself determines what resources are needed.
        if matches!(&contract, ToolContract::SemanticGraphQuery(_)) {
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

        let (result, is_error) = match contract {
            ToolContract::ProjectLifecycle => self.handle_project(arguments),
            ToolContract::StatusRead => self.dispatch_status_read(ctx, name, arguments),
            ToolContract::SemanticGraphQuery(_) => self.dispatch_graph_query(ctx, name, arguments),
            ToolContract::TraceQuery(_) => self.dispatch_trace_query(ctx, name, arguments),
            ToolContract::StoreFactQuery(_) => self.dispatch_store_query(ctx, name, arguments),
            ToolContract::SemanticAnalysis(_) => self.dispatch_analysis(ctx, name, arguments),
            ToolContract::OverlayMutation(_) | ToolContract::OverlayRead => {
                self.dispatch_overlay(ctx, name, arguments)
            }
            ToolContract::TaskControl => self.dispatch_task_control(ctx, name, arguments),
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

    /// Sub-dispatcher: `StatusRead` contract tools.
    fn dispatch_status_read(
        &self,
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
        &self,
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
        &self,
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
        &self,
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
        &self,
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
    fn dispatch_overlay(&self, _ctx: &ToolCallContext, name: &str, args: &Value) -> (String, bool) {
        match name {
            "fp_dispatches" => self.handle_fp_dispatches(args),
            "domain_rules" => self.handle_domain_rules(args),
            _ => (format!("Unknown overlay tool: {name}"), true),
        }
    }

    /// Sub-dispatcher: `TaskControl` contract tools.
    fn dispatch_task_control(
        &self,
        _ctx: &ToolCallContext,
        name: &str,
        args: &Value,
    ) -> (String, bool) {
        match name {
            "tasks" => self.handle_tasks(args),
            "resume_query" => self.handle_resume_query(args),
            _ => (format!("Unknown task tool: {name}"), true),
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
                log_include_roots_warnings(&warnings);
                return (roots, warnings);
            }
            None => {
                log_include_roots_warnings(&warnings);
                return (roots, warnings);
            }
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
            if !self.project().root.join(&normalized).is_dir() {
                warnings.push(format!(
                    "include_roots: directory not found (used anyway): {normalized}"
                ));
            }

            if seen.insert(normalized.clone()) {
                roots.push(atlas_engine::IncludeRoot { path: normalized });
            }
        }

        log_include_roots_warnings(&warnings);
        (roots, warnings)
    }

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
    pub(crate) fn store_snapshot(&self, snapshot: QuerySnapshot) {
        self.project().job_runtime.store_snapshot(snapshot);
    }

    /// Update or create investigation based on a tool call focus.
    pub(crate) fn update_investigation(&self, focus: atlas_engine::InvestigationFocus) {
        self.project().job_runtime.update_investigation(focus);
    }
}

// Implement SnapshotStore for ToolRouter so AnalysisEnvelope::build() can store
// snapshots without knowing the concrete handler type.
impl SnapshotStore for ToolRouter {
    fn store_query_snapshot(&self, snapshot: QuerySnapshot) {
        self.store_snapshot(snapshot);
    }
}

// -------------------------------------------------------------------
// Shared helper functions (module-level, not on ToolRouter)
// -------------------------------------------------------------------

/// Validates that a symbol name does not exceed the maximum length.
/// Returns `Err(message)` if too long.
pub(crate) fn validate_symbol_name_length(name: &str) -> Result<(), String> {
    if name.len() > MAX_SYMBOL_NAME_LENGTH {
        Err(format!(
            "symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"
        ))
    } else {
        Ok(())
    }
}

/// Format an "ambiguous symbol" error message for display.
///
/// Produces a human-readable error listing up to 5 candidate matches
/// with file, line, and kind information.
pub(crate) fn format_ambiguous_error(
    candidates: &[ScoredCandidate],
    symbol_name: &str,
) -> (String, bool) {
    let candidates_str: Vec<String> = candidates
        .iter()
        .take(5)
        .map(|c| format!("{}::{} [{}]", c.file_path, c.line, c.kind))
        .collect();
    (
        format!(
            "Symbol '{}' is ambiguous ({} matches: {}). Use a SymbolSelector object from search results (symbol_ref field).",
            symbol_name,
            candidates.len(),
            candidates_str.join(", ")
        ),
        true,
    )
}

/// Log include_roots warnings through the tracing infrastructure.
///
/// Called by [`ToolRouter::include_roots_from_args`] at every return point
/// so callers never need to manually iterate and log warnings.
fn log_include_roots_warnings(warnings: &[String]) {
    for w in warnings {
        tracing::warn!("include_roots: {}", w);
    }
}

/// Render a graph node as a JSON object.
///
/// Requires a [`StoreQueryRuntime`] to resolve `file_id → path`.
pub(crate) fn node_json(
    store_query: &crate::tools::runtime::store_query_runtime::StoreQueryRuntime,
    snap: &atlas_engine::GraphSnapshot,
    ix: atlas_engine::NodeIx,
    edge_kind: Option<&str>,
) -> Value {
    let n = snap.node(ix);
    // signature from store by symbol_id (not GraphSnapshot NodeSummary — keeps graph lean).
    let signature = store_query
        .store
        .find_symbol_by_id(&n.symbol_id)
        .ok()
        .flatten()
        .and_then(|s| s.signature);
    let mut obj = json!({
        "name": n.name,
        "qualified_name": n.qualified_name,
        "kind": n.kind.as_str(),
        "file": store_query.resolve_file_path(&n.file_id),
        "line": n.start_line,
        "signature": signature,
    });
    if let Some(ek) = edge_kind {
        obj["edge"] = json!(ek);
    }
    obj
}

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
// Tool registration — 15 tools (open-first focus MCP surface)
// ===================================================================

// ── Project tools ────────────────────────────────────────────────────

fn make_project_tools() -> Vec<Tool> {
    vec![Tool {
        name: "project".into(),
        description: "Open, inspect, or list files in a project. Use action='open' to synchronously activate a project backed by project/.atlas/atlas.db; MCP open never indexes or scans the whole tree. Explicit indexing is CLI-only (`atlas index`). action='status' reports the active project and focus state; action='files' lists known project files.".into(),
        input_schema: ToolInputSchema {
            schema_type: "object".into(),
            properties: Some(json!({
                "action": {
                    "type": "string",
                    "enum": ["open", "status", "files"],
                    "description": "Operation: 'open' activates a project, 'status' shows overview, 'files' lists known project files."
                },
                "project_path": { "type": "string", "description": "Absolute path to the project directory to open (required for action='open')." },
                "verbose": { "type": "boolean", "description": "Include verbose details (action='status')." },
                "limit": { "type": "integer", "description": "Max files returned (action='files', default unlimited)." },
                "language": { "type": "string", "description": "Filter files by language (action='files', e.g. 'rust', 'typescript')." },
                "path_prefix": { "type": "string", "description": "Filter files by path prefix (action='files')." },
            })),
            required: None,
        },
    }]
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
            description: "Search symbols by name within a required project-relative scope. Scope is always required because it is both the result boundary and the focus seed; an existing CLI-built full index improves precision/performance but does not make scope optional.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query": { "type": "string", "description": "Search query text" },
                    "scope": { "type": "string", "description": "Required project-relative directory or file scope (e.g. 'drivers/net', 'src', 'kernel/sched'). Defines the search boundary and focus hotspot." },
                    "kind": { "type": "string", "description": "Optional SymbolKind filter (function, class, ...)" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["query".into(), "scope".into()]),
            },
        },
        Tool {
            name: "symbol".into(),
            description: "Get symbol information by qualified name (symbol). view='detail' returns kind, location, and signature (with optional source via includeCode). view='context' returns structured callers, callees, file peers, imports, and dependencies. view='usages' returns reference usages. Default view is 'detail'.".into(),
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
            description: "Query the call graph around a symbol. direction='incoming' (callers) and 'outgoing' (callees) are fixed 1-hop and include signature when available; depth is ignored (warning). direction='both' enables multi-hop via depth (default 1, max 5). edge_kinds defaults to [\"calls\",\"instantiates\",\"implements\"]; use [\"*\"] for all kinds.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches are auto-aggregated. Use SymbolSelector object for a precise single-symbol query."),
                    "direction": {
                        "type": "string",
                        "enum": ["incoming", "outgoing", "both"],
                        "description": "Edge direction: 'incoming' for callers (1-hop), 'outgoing' for callees (1-hop), 'both' for multi-hop when depth>1 (default 'both')."
                    },
                    "depth": { "type": "integer", "description": "Only for direction=both: traversal depth (default 1, max 5). Ignored for incoming/outgoing (1-hop only)." },
                    "limit": { "type": "integer", "description": "Max nodes returned (default depends on mode)." },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\",\"instantiates\",\"implements\"]. Use [\"*\"] or [] for all edge kinds (neighbor query mode)."
                    },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
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
                    "scope": { "type": "string", "description": "Optional project-relative directory or file scope for cold/local exploration (e.g. drivers/hid, net/smc). Keeps first-pass analysis bounded to the requested region." },
                    "source_mode": { "type": "string", "enum": ["excerpt", "full", "none"], "description": "Source display mode: excerpt (snippet around definition), full (entire symbol body, capped by max_source_bytes=65536), none (skip source). Default: excerpt." },
                    "source_lines": { "type": "integer", "description": "Max source lines to return when source_mode=excerpt. Default: 40." },
                    "evidence_limit": { "type": "integer", "description": "Max call evidence examples per direction. Default: 5." },
                    "relation_limit": { "type": "integer", "description": "Max non-call relation examples across all groups. Default: 12." },
                    "peer_limit": { "type": "integer", "description": "Max file peer symbols to return. Default: 12." },
                    "include_file_context": { "type": "boolean", "description": "Include imports, exports, and file peers. Default: true." },
                    "include_recommendations": { "type": "boolean", "description": "Include recommended next queries. Default: true." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
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
            description: "Compute impact analysis: all symbols reachable from a given symbol via call graph traversal. Use direction='both' for bidirectional (downstream + upstream), direction='incoming' for callers only. Use semantic=true to include lifecycle invariants and branch diffs for impacted functions.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches are auto-aggregated."),
                    "direction": {
                        "type": "string",
                        "enum": ["outgoing", "incoming", "both"],
                        "description": "Traversal direction. 'outgoing' (default) follows forward/call edges only (downstream effects). 'incoming' follows reverse/caller edges only. 'both' follows both directions for full impact radius."
                    },
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
                                "description": "Resolve a source position (file+line+column) to its full context — enclosing symbol, reference, scope, data node, and callsite. Triggers scoped structural/dataflow preparation when needed; capability gaps are reported in the response."
                            },
                            {
                                "const": "variable",
                                "description": "Trace where a variable's value comes from (backward intra-procedural dataflow). Requires dataflow layer for complete results; returns best-effort on structural-only projects."
                            },
                            {
                                "const": "forward",
                                "description": "Trace the forward call chain from source symbol to target symbol. Scoped focus prepares call-graph edges when needed; partial coverage is reported in the response."
                            },
                            {
                                "const": "callers",
                                "description": "Trace how a function gets invoked — backward call chain to the farthest caller. Scoped focus prepares call-graph edges when needed; partial coverage is reported in the response."
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
            description: "Manage function-pointer dispatch annotations for C/C++ code. action='add' declares a mapping from a struct's function-pointer field to its concrete target function (required: field_qname, target_qname). action='list' returns all declared annotations. action='delete' removes an annotation (required: annotation_id OR field_qname). Annotations are stored in the active project database; graph edges are materialized immediately.".into(),
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

// ── Query/job tools ──────────────────────────────────────────────────

fn make_task_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "tasks".into(),
            description: "List focus/lazy extraction jobs and query refinement state. Without arguments, lists all active jobs. Use query_id to filter refinement work triggered by a specific query.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query_id": { "type": "string", "description": "Optional query_id to filter jobs." },
                })),
                required: None,
            },
        },
        Tool {
            name: "resume_query".into(),
            description: "Re-run a previous query snapshot to get enhanced results after focus/lazy refinement. Returns the same format as the original tool with potentially richer data.".into(),
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
// Facade handlers — dispatch merged tools to internal sub-handlers
// ===================================================================

impl ToolRouter {
    // ── project ──────────────────────────────────────────────────────

    /// Handle `project` tool — dispatch by `action`.
    pub(crate) fn handle_project(&self, args: &Value) -> (String, bool) {
        let action = get_str(args, "action");
        match action {
            "open" => self.handle_open_project(args),
            "status" if self.project.is_active() => self.handle_status(),
            "status" => (
                serde_json::to_string_pretty(&json!({
                    "state": "not_open",
                    "active_project": null,
                    "open_required": true,
                    "message": "Open a project before using code-analysis tools."
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
            "files" if self.project.is_active() => self.handle_files(args),
            "files" => (
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "error": "No active project. Call project(action=\"open\") first."
                }))
                .unwrap_or_else(|e| e.to_string()),
                true,
            ),
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

    /// Handle `symbol` tool — dispatch by `view` to sub-handlers.
    /// Remaps `symbol` → `qualified_name` (detail) or passes through as `symbol` (context/usages).
    pub(crate) fn handle_symbol(&self, ctx: &ToolCallContext, args: &Value) -> (String, bool) {
        // Position-based lookup: file_path + line as alternative to 'symbol'
        let file_path = get_str(args, "file_path");
        let line_opt = args.get("line").and_then(|v| v.as_u64()).map(|v| v as u32);
        if let Some(line) = line_opt.filter(|_| !file_path.is_empty()) {
            return self.handle_symbol_by_position(ctx, file_path, line, args);
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
                    args.get("symbol")
                        .cloned()
                        .unwrap_or(Value::String(qname.clone())),
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
                    args.get("symbol")
                        .cloned()
                        .unwrap_or(Value::String(qname.clone())),
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
                    args.get("symbol")
                        .cloned()
                        .unwrap_or(Value::String(qname.clone())),
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

pub(crate) fn resolve_calls_dispatch(args: &serde_json::Value) -> CallsDispatch {
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
    let is_default_edges = !is_wildcard && edge_kinds == ["calls", "instantiates", "implements"];
    let is_custom_edges = !is_wildcard && !is_default_edges;

    if is_custom_edges || is_wildcard || depth > 1 || direction == "both" || direction.is_empty() {
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
    pub(crate) fn handle_calls(&self, args: &Value) -> (String, bool) {
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
    pub(crate) fn handle_file_dependencies(&self, args: &Value) -> (String, bool) {
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

        // Resolve file_path to file_id for sub-handlers
        let clean = file_path.trim_start_matches("./").trim_start_matches('/');
        let file_id = {
            let active = self.project();
            match active.store.resolve_file_id(&active.root, clean) {
                Ok(Some(id)) => id,
                Ok(None) => return (format!("File not found: {file_path}"), true),
                Err(e) => return (format!("Failed to resolve file: {e}"), true),
            }
        };

        if is_manifest {
            return self.handle_file_dependencies_manifest(file_id, direction, args);
        }

        // ── structural mode ─────────────────────────────────────────────
        let mut lazy_warnings = Vec::new();
        let mut built_file_count = 0usize;
        let mut focus_result = None;
        let mut _capability_mask = atlas_engine::structs::FactCoverage::default();
        let mut _coverage = "full";
        let mut _reason: Option<&str> = None;

        let has_full_index = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };
        if !has_full_index {
            // Construct a Calls intent with the resolved file_id so focus
            // extraction uses a FocusSeed::File seed.  No QueryIntent variant
            // accepts multiple file_ids (e.g. edge-dependent candidates), so
            // we scope extraction to the primary file.  The closure engine
            // expands via CallGraph + ImportNeighborhood strategies which is
            // appropriate for structural dependency extraction.  (P1-F6)
            let intent = Some(atlas_engine::QueryIntent::Calls {
                symbol_name: String::new(),
                file_id: Some(file_id),
                symbol_id: None,
                direction: None,
                depth: None,
            });
            let (prepared, focus_warnings) = self.prepare_focus_query(intent);
            lazy_warnings = focus_warnings;
            built_file_count = prepared.as_ref().map(|r| r.built_files.len()).unwrap_or(0);
            focus_result = prepared;
        } else {
            _capability_mask = self.project().store.derive_capability_for_files(&[file_id]);
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
                    format!("Lazy-built {built_file_count} files (structural mode)")
                } else {
                    "Full index available".into()
                };
                let lr = AnalysisEnvelope::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err);
                let lr = if let Some(ref result) = focus_result {
                    crate::tools::apply_focus_result_to_lr(lr, result)
                } else {
                    lr.with_analysis_scope("structural".into())
                        .with_analysis_summary(summary)
                };
                lr.build(body, self)
            }
            "outgoing" | "" => {
                let (out, err) = self.handle_dependencies(&mapped_args);
                let body = serde_json::from_str::<Value>(&out).unwrap_or_default();
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {built_file_count} files (structural mode)")
                } else {
                    "Full index available".into()
                };
                let lr = AnalysisEnvelope::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err);
                let lr = if let Some(ref result) = focus_result {
                    crate::tools::apply_focus_result_to_lr(lr, result)
                } else {
                    lr.with_analysis_scope("structural".into())
                        .with_analysis_summary(summary)
                };
                lr.build(body, self)
            }
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&mapped_args);
                let (in_str, in_err) = self.handle_dependents(&mapped_args);
                let body = json!({
                    "outgoing": serde_json::from_str::<Value>(&out_str).unwrap_or_default(),
                    "incoming": serde_json::from_str::<Value>(&in_str).unwrap_or_default(),
                });
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {built_file_count} files (structural mode)")
                } else {
                    "Full index available".into()
                };
                let err = out_err || in_err;
                let lr = AnalysisEnvelope::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err);
                let lr = if let Some(ref result) = focus_result {
                    crate::tools::apply_focus_result_to_lr(lr, result)
                } else {
                    lr.with_analysis_scope("structural".into())
                        .with_analysis_summary(summary)
                };
                lr.build(body, self)
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
                let resp =
                    add_manifest_analysis(serde_json::to_string_pretty(&value).unwrap_or_default());
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
                let resp =
                    add_manifest_analysis(serde_json::to_string_pretty(&value).unwrap_or_default());
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
                merge_edge_deps(
                    &mut outgoing,
                    &edge_out,
                    "dependencies",
                    "total_dependencies",
                );
                merge_edge_deps(&mut incoming, &edge_in, "dependents", "total_dependents");

                let result = json!({
                    "outgoing": outgoing,
                    "incoming": incoming,
                    "analysis": manifest_analysis_value(),
                });
                (
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                    err,
                )
            }
            _ => unreachable!("direction was validated above"),
        }
    }

    /// Query symbol_edges for incoming file dependencies (manifest mode).
    /// Returns files whose symbols have edges targeting symbols in `file_id`.
    fn manifest_edge_dependents(&self, file_id: &FileId, max_results: usize) -> Value {
        if max_results == 0 {
            return json!([]);
        }
        let our_symbols = match self.project().store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.project().store.find_edges_for_files(&[*file_id]) {
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
        let symbols = match self.project().store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        let mut file_paths: HashSet<String> = HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for sym in &symbols {
            if file_paths.len() >= max_results {
                break;
            }
            let path = self
                .project()
                .store_query_runtime
                .resolve_file_path(&sym.file_id);
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
        let our_symbols = match self.project().store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.project().store.find_edges_for_files(&[*file_id]) {
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
        let symbols = match self.project().store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        let mut file_paths: HashSet<String> = HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for sym in &symbols {
            if file_paths.len() >= max_results {
                break;
            }
            let path = self
                .project()
                .store_query_runtime
                .resolve_file_path(&sym.file_id);
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
    pub(crate) fn handle_fp_dispatches(&self, args: &Value) -> (String, bool) {
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
    pub(crate) fn handle_domain_rules(&self, args: &Value) -> (String, bool) {
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
    pub(crate) fn handle_tasks(&self, args: &Value) -> (String, bool) {
        let query_id = get_str_opt(args, "query_id");

        let query = query_id.map(|qid| {
            self.project().job_runtime.prune_expired_snapshots();
            let snapshot = self
                .project()
                .job_runtime
                .query_snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(qid)
                .cloned();
            let Some(snapshot) = snapshot else {
                return json!({
                    "query_id": qid,
                    "status": "not_found_or_expired",
                    "pending_jobs": 0,
                });
            };

            let (pending, retry_after_ms) = snapshot
                .focus_result
                .as_ref()
                .map(|result| self.focus_pending_count_and_eta_ms(result))
                .unwrap_or((0, 0));
            let mut state = json!({
                "query_id": qid,
                "tool": snapshot.tool_name,
                "status": if pending == 0 { "ready" } else { "refining" },
                "pending_jobs": pending,
            });
            if pending > 0 {
                state["retry_after_ms"] = json!(retry_after_ms);
            }
            state
        });

        let (jobs_str, jobs_err) = self.handle_jobs();
        let atlas_args = if let Some(qid) = query_id {
            let mut m = serde_json::Map::new();
            m.insert("query_id".into(), Value::String(qid.to_string()));
            Value::Object(m)
        } else {
            Value::Object(serde_json::Map::new())
        };
        let (atlas_str, atlas_err) = self.handle_atlas_jobs(&atlas_args);

        let mut result = json!({
            "active_extraction_jobs": serde_json::from_str::<Value>(&jobs_str).unwrap_or_default(),
            "atlas_jobs": serde_json::from_str::<Value>(&atlas_str).unwrap_or_default(),
        });
        if let Some(query) = query {
            result["query"] = query;
        }
        (
            serde_json::to_string_pretty(&result).unwrap_or_default(),
            jobs_err || atlas_err,
        )
    }
}

// -------------------------------------------------------------------
// Shared arg-parsing helpers
// -------------------------------------------------------------------

/// Add the unified analysis block for manifest-mode responses.
fn add_manifest_analysis(response: String) -> String {
    let mut value = serde_json::from_str::<Value>(&response).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("analysis".into(), manifest_analysis_value());
    }
    serde_json::to_string_pretty(&value).unwrap_or(response)
}

fn manifest_analysis_value() -> Value {
    json!({
        "scope": "local",
        "basis": ["manifest"],
        "summary": "Manifest file dependency facts are available for this file.",
    })
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
        &self,
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
        if self
            .project()
            .store
            .get_file(&file_id)
            .ok()
            .flatten()
            .is_none()
        {
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
        let (_focus_result, focus_warnings) =
            self.prepare_focus_query_with_roots(None, include_roots);
        let mut warnings: Vec<String> = root_warnings;
        warnings.extend(focus_warnings);

        // Find all symbols in the file
        let symbols = match self.project().store.find_symbols_by_file(&file_id) {
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
        let view = args.get("view").and_then(|v| v.as_str()).unwrap_or("");

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
                    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&result) {
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
                format!("Unknown view: '{other}'. Must be one of: detail, context, usages"),
                true,
            ),
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;
