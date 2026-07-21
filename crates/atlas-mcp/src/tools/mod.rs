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
use std::time::{Duration, Instant};

use super::protocol::{CallToolResult, ContentBlock, ListToolsResult, Tool, ToolInputSchema};

use serde_json::{Value, json};
use std::sync::atomic::Ordering;

use crate::tools::analysis_envelope::{AnalysisEnvelope, SnapshotStore};
use crate::tools::query_snapshot::QuerySnapshot;
#[cfg(test)]
use crate::tools::runtime::graph_runtime::EdgeProvenance;
use symbol_selector::ScoredCandidate;

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
pub(crate) mod file_deps;
pub(crate) mod graph;
#[cfg(test)]
mod handler_purity;
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
pub(crate) mod tool_schemas;
pub(crate) mod trace;
pub(crate) mod usages;

// Re-exports to preserve access paths used by sibling modules and lib.rs.
pub(crate) use graph::{CallsDispatch, resolve_calls_dispatch};
pub use tool_schemas::make_all_tools;
// Test-only re-exports so mod_tests.rs can reach these via `use super::*;`.
#[cfg(test)]
pub(crate) use tool_schemas::{make_trace_tools, merge_edge_deps};

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
            tools: tool_schemas::make_all_tools(),
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
            tools: tool_schemas::make_all_tools(),
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
            tools: tool_schemas::make_all_tools(),
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

    fn query_pending_count_and_eta_ms(&self, query_id: &str) -> Option<(usize, u64)> {
        let snapshot = self
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(query_id)
            .cloned()?;
        snapshot
            .focus_result
            .as_ref()
            .map(|result| self.focus_pending_count_and_eta_ms(result))
    }

    fn response_retry_state(response: &str) -> Option<(String, String, String)> {
        let value: Value = serde_json::from_str(response).ok()?;
        value.get("analysis")?.get("retry_after_ms")?.as_u64()?;
        let query_id = value.get("query_id")?.as_str()?.to_string();
        let scope = value
            .get("analysis")
            .and_then(|analysis| analysis.get("scope"))
            .and_then(Value::as_str)
            .unwrap_or("local")
            .to_string();
        let detail = value
            .get("analysis")
            .and_then(|analysis| analysis.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or("Focus is still materializing facts required by this query.")
            .to_string();
        Some((query_id, scope, detail))
    }

    fn response_query_id(response: &str) -> Option<String> {
        serde_json::from_str::<Value>(response)
            .ok()?
            .get("query_id")?
            .as_str()
            .map(str::to_string)
    }

    fn query_focus_failure(&self, query_id: &str) -> Option<String> {
        let snapshot = self
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(query_id)
            .cloned()?;
        let result = snapshot.focus_result.as_ref()?;
        let failures = result
            .job_tracker
            .as_ref()?
            .failures_for(&result.pending_closure_ids);
        if failures.is_empty() {
            return None;
        }
        Some(
            failures
                .into_iter()
                .map(|(job_id, reason)| format!("{job_id}: {reason}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    fn query_need_for_response(
        &self,
        contract: &ToolContract,
        query_id: &str,
    ) -> Option<atlas_engine::QueryNeed> {
        contract.query_need().or_else(|| {
            let snapshot = self
                .project()
                .job_runtime
                .query_snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(query_id)
                .cloned()?;
            contract_for(&snapshot.tool_name, &snapshot.tool_args).query_need()
        })
    }

    fn original_tool_name(&self, query_id: &str, fallback: &str) -> String {
        self.project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(query_id)
            .map(|snapshot| snapshot.tool_name.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    fn strict_pending_ticket(
        &self,
        tool_name: &str,
        query_id: &str,
        scope: &str,
        detail: &str,
        need: atlas_engine::QueryNeed,
    ) -> String {
        let retry_after_ms = self
            .query_pending_count_and_eta_ms(query_id)
            .map(|(_, eta)| eta)
            .filter(|eta| *eta > 0)
            .unwrap_or(500);
        serde_json::to_string_pretty(&json!({
            "status": "in_progress",
            "tool": tool_name,
            "query_id": query_id,
            "pending": {
                "reason": format!("focus_{}_not_ready", need.as_str()),
                "required_analysis": need.as_str(),
                "detail": detail,
            },
            "analysis": {
                "scope": scope,
                "summary": format!(
                    "Focus is still building the {} facts required for this query; no partial result is published.",
                    need.as_str()
                ),
                "retry_after_ms": retry_after_ms,
            }
        }))
        .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
    }

    fn strict_failed_ticket(
        &self,
        tool_name: &str,
        query_id: &str,
        need: atlas_engine::QueryNeed,
        detail: &str,
    ) -> String {
        serde_json::to_string_pretty(&json!({
            "status": "failed",
            "tool": tool_name,
            "query_id": query_id,
            "pending": {
                "reason": format!("focus_{}_failed", need.as_str()),
                "required_analysis": need.as_str(),
                "detail": detail,
            },
            "analysis": {
                "scope": "local",
                "summary": format!(
                    "Focus could not finish the {} facts required for this query; no partial result is published. Re-run the original query to retry materialization.",
                    need.as_str()
                ),
            }
        }))
        .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
    }

    /// Wait within the single interactive deadline and replay the existing
    /// query snapshot when tracked Focus work finishes. If the deadline wins,
    /// publish only a resumable ticket—never the handler's provisional data.
    fn settle_focus_response(
        &self,
        contract: &ToolContract,
        tool_name: &str,
        mut response: String,
        mut is_error: bool,
        started_at: Instant,
    ) -> (String, bool) {
        let deadline = started_at
            .checked_add(Duration::from_millis(
                atlas_engine::INTERACTIVE_QUERY_BUDGET_MS,
            ))
            .unwrap_or(started_at);

        loop {
            let Some((query_id, scope, detail)) = Self::response_retry_state(&response) else {
                let Some(query_id) = Self::response_query_id(&response) else {
                    return (response, is_error);
                };
                let Some(failure) = self.query_focus_failure(&query_id) else {
                    return (response, is_error);
                };
                let Some(need) = self.query_need_for_response(contract, &query_id) else {
                    return (response, is_error);
                };
                let original_tool_name = self.original_tool_name(&query_id, tool_name);
                return (
                    self.strict_failed_ticket(&original_tool_name, &query_id, need, &failure),
                    true,
                );
            };
            let Some(need) = self.query_need_for_response(contract, &query_id) else {
                return (response, is_error);
            };
            let original_tool_name = self.original_tool_name(&query_id, tool_name);

            let now = Instant::now();
            if now >= deadline {
                return (
                    self.strict_pending_ticket(
                        &original_tool_name,
                        &query_id,
                        &scope,
                        &detail,
                        need,
                    ),
                    false,
                );
            }

            match self.query_pending_count_and_eta_ms(&query_id) {
                Some((0, _)) => {
                    let resumed = self.handle_resume_query(&json!({"query_id": query_id}));
                    response = resumed.0;
                    is_error = resumed.1;
                }
                Some(_) => {
                    let remaining = deadline.saturating_duration_since(now);
                    std::thread::sleep(remaining.min(Duration::from_millis(50)));
                }
                None => {
                    return (
                        self.strict_pending_ticket(
                            &original_tool_name,
                            &query_id,
                            &scope,
                            &detail,
                            need,
                        ),
                        false,
                    );
                }
            }
        }
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

        // No intent means there is no query strength from which to decide
        // whether a pre-index is sufficient.
        let intent = match intent {
            Some(i) => i,
            None => return (None, vec![]),
        };

        let project = self.project();
        // QueryRuntime performs the QueryNeed-aware pre-index check before
        // entering Focus.
        let (focus_result, warnings) =
            project
                .query_runtime
                .prepare(&intent, &project.store, include_roots);

        // Post-processing: record lazy writes and refresh graph.
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
    pub(crate) fn enqueue_background_file_focus(
        &self,
        file_ids: &[FileId],
    ) -> Option<atlas_engine::focus::runtime::FocusResult> {
        if file_ids.is_empty() {
            return None;
        }

        let project = self.project();
        match project.query_runtime.enqueue_file_focus_warm(file_ids) {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!("background focus warming enqueue failed: {err:#}");
                None
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

    /// Find files that may reference a symbol or module name. Dependency
    /// queries use this to warm likely importers without synchronously building
    /// a project-wide closure.
    pub(crate) fn candidate_file_ids_referencing(&self, name: &str) -> Vec<FileId> {
        let project = self.project();
        let provider =
            DefaultCandidateProvider::new(project.store.clone(), Some(project.root.clone()));
        let mut seen = HashSet::new();
        provider
            .candidates_for_references(name)
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
        let background_focus = self.enqueue_background_file_focus(&file_ids);
        let (pending_count, retry_after_ms) = background_focus
            .as_ref()
            .map(|result| result.pending_work_count_and_eta_ms())
            .unwrap_or((0, 0));

        let message = if pending_count > 0 {
            "The symbol is not available in the current local focus closure yet. Background scoped analysis has been started; retry this request after the suggested delay, or pass a SymbolSelector with file_path/scope to constrain the local region."
        } else {
            "The symbol is not available in the current local focus closure, and no candidate refinement remains pending. Pass a SymbolSelector with file_path/scope to constrain a different local region."
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

        let mut lr = AnalysisEnvelope::new(tool_name, args)
            .with_is_error(false)
            .with_analysis_scope("local".into())
            .with_analysis_summary(
                if pending_count > 0 {
                    "bounded unresolved result; background scoped analysis is preparing local symbol facts"
                } else {
                    "bounded unresolved result; no further background refinement is pending"
                }
                .into(),
            )
            .with_analysis_basis(vec!["manifest".into(), "structural".into()]);
        if pending_count > 0 {
            lr = lr.with_analysis_retry_after_ms(retry_after_ms);
        } else {
            lr = lr.with_gap_records(vec![analysis_envelope::GapRecord {
                scope: symbol.to_string(),
                reason: "symbol_not_materialized".into(),
                detail: "No matching symbol was found in the available local structural facts, and no candidate refinement remains pending."
                    .into(),
            }]);
        }
        if let Some(result) = background_focus {
            lr = lr.with_focus_result(result);
        }
        lr.build(resp, self)
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
            // Re-check QueryNeed-specific repo-cache eligibility (layer
            // distribution may have changed after Index/sync or Focus writes).
            *project
                .query_runtime
                .cache
                .cached_repo_cache
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
                .cached_repo_cache
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
                .cached_repo_cache
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
        let started_at = Instant::now();

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

        let (result, is_error) = match &contract {
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
        let (result, is_error) =
            self.settle_focus_response(&contract, name, result, is_error, started_at);

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

// -------------------------------------------------------------------
// Shared arg-parsing helpers
// -------------------------------------------------------------------

/// Add the unified analysis block for manifest-mode responses.
pub(crate) fn add_manifest_analysis(response: String) -> String {
    let mut value = serde_json::from_str::<Value>(&response).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("analysis".into(), manifest_analysis_value());
    }
    serde_json::to_string_pretty(&value).unwrap_or(response)
}

pub(crate) fn manifest_analysis_value() -> Value {
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
pub(crate) fn normalize_project_relative_path(raw: &str) -> Option<String> {
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
pub(crate) fn is_definition_kind(_kind: &atlas_engine::SymbolKind) -> bool {
    // All current SymbolKind values (File, Module, Class, Struct,
    // Interface, Trait, Enum, EnumMember, Function, Method, Property,
    // Field, Variable, Constant, TypeAlias, Namespace, Parameter,
    // Constructor, Macro, Decorator, Package) are definitions.
    true
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;
