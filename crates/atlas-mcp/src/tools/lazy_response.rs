//! Unified lazy extraction diagnostics and response envelope for MCP tool responses.
//!
//! Provides [`LazyDiagnostics`] — a consistent UX contract surfaced in every
//! tool response that triggers lazy extraction.  Handlers construct diagnostics
//! from [`LazyOutcome`] (structural) and [`LazyWindow`] (dataflow), and the
//! serialized JSON includes a `lazy_diagnostics` block with per-layer stats
//! and a recommended next action.
//!
//! Also provides [`LazyResponse`] — a builder that centralizes the common
//! response envelope pattern shared by "full envelope" tool handlers:
//! generating a `query_id`, merging warnings, adding
//! `lazy_diagnostics`/`analysis_contract`,
//! and storing a [`super::query_snapshot::QuerySnapshot`].

use std::collections::HashMap;

use atlas_engine::LazyOutcome;
use atlas_engine::LazyWindow;
use atlas_engine::structs::CapabilityMask;
use atlas_engine::structs::CoverageTier;
use atlas_engine::structs::KnownGap;
use atlas_engine::structs::Precision;
use atlas_engine::structs::SemanticConfidence;
use atlas_engine::structs::precision::PrecisionTier;
use serde::Serialize;
use serde_json::json;

use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::analysis_response::{WorkItem, WorkProgress};

/// Unified lazy extraction diagnostics for MCP tool responses.
///
/// Every response from a handler that triggers lazy extraction surfaces a
/// `lazy_diagnostics` block so agents can understand extraction state and
/// decide what to do next (poll jobs, narrow scope, run a full index).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LazyDiagnostics {
    /// Structural layer diagnostics (None if no lazy structural ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) structural: Option<LazyLayerDiagnostics>,
    /// Dataflow layer diagnostics (None if no lazy dataflow ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dataflow: Option<LazyLayerDiagnostics>,
    /// Recommended next action for the user/agent.
    pub(crate) next_action: &'static str,
    /// Analysis contract: what conclusions are safe/unsafe given current extraction state.
    pub(crate) analysis_contract: AnalysisContract,
}

/// Per-layer lazy extraction diagnostics.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LazyLayerDiagnostics {
    /// Whether lazy extraction was triggered for this layer.
    pub(crate) triggered: bool,
    /// Number of files/units that were successfully built.
    pub(crate) files_built: usize,
    /// Number of files/units that were already cached (skipped).
    pub(crate) files_cached: usize,
    /// Number of files/units being built by another request (in-flight).
    pub(crate) files_pending: usize,
    /// Whether the budget was exceeded (results may be incomplete).
    pub(crate) budget_exceeded: bool,
}

// ── Analysis Contract ───────────────────────────────────────────────────

/// Analysis contract: what conclusions are safe/unsafe given current extraction state.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalysisContract {
    pub safe_conclusions: Vec<String>,
    pub unsafe_conclusions: Vec<String>,
    pub capability_summary: CapabilitySummary,
    pub refinement_jobs: Vec<RefinementJob>,
}

/// Summary of capability masks available across the project.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CapabilitySummary {
    pub mask_bits: u16,
    pub best_capability: String,
    pub total_files: usize,
    pub files_with_dataflow: usize,
    pub files_with_cfg: usize,
    pub files_structural_only: usize,
    pub files_manifest_only: usize,
}

/// Optional project-wide capability statistics populated from the DB.
/// When `None`, all counts default to 0 (the caller hasn't queried yet).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CapabilityStats {
    pub files_with_dataflow: usize,
    pub files_structural_only: usize,
    pub files_manifest_only: usize,
    pub files_with_cfg: usize,
}

/// Derive an actual capability mask from DB-sourced file counts.
/// Only sets bits that have at least one verified file.
fn capability_mask_from_counts(stats: &CapabilityStats) -> CapabilityMask {
    let mut mask = CapabilityMask::default();
    // Manifest: any file that's been indexed at all
    let total = stats.files_with_dataflow
        + stats.files_structural_only
        + stats.files_manifest_only
        + stats.files_with_cfg;
    if total > 0 {
        mask = CapabilityMask::new(mask.bits() | CapabilityMask::MANIFEST);
    }
    // Structural + Call edges: at least structural tier
    if stats.files_structural_only + stats.files_with_dataflow + stats.files_with_cfg > 0 {
        mask = CapabilityMask::new(
            mask.bits() | CapabilityMask::STRUCTURAL | CapabilityMask::CALL_EDGES,
        );
    }
    // Dataflow
    if stats.files_with_dataflow > 0 {
        mask = CapabilityMask::new(mask.bits() | CapabilityMask::DATAFLOW);
    }
    // CFG
    if stats.files_with_cfg > 0 {
        mask = CapabilityMask::new(mask.bits() | CapabilityMask::CFG);
    }
    // SUMMARIES: implied by dataflow (verified through dataflow count)
    if stats.files_with_dataflow > 0 {
        mask = CapabilityMask::new(mask.bits() | CapabilityMask::SUMMARIES);
    }
    mask
}

/// A background job that would improve the analysis contract.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RefinementJob {
    pub description: String,
    pub capability_needed: String,
}

impl AnalysisContract {
    /// Build an AnalysisContract from a capability mask and optional LazyOutcome.
    /// `capability_stats` populates file-count breakdowns; pass `None` for zero defaults.
    ///
    /// When `capability_stats` is provided, the theoretical `mask` is AND‑reconciled
    /// with the actual DB file counts so that a bit is only claimed if at least one
    /// file has been verified at that tier.
    pub(crate) fn from_capability(
        mask: CapabilityMask,
        outcome: Option<&LazyOutcome>,
        capability_stats: Option<CapabilityStats>,
    ) -> Self {
        // Reconcile theoretical mask with actual DB state.
        // Without stats we retain the mask as-is (backward compatible).
        let effective_mask = if let Some(ref stats) = capability_stats {
            let actual_mask = capability_mask_from_counts(stats);
            CapabilityMask::new(mask.bits() & actual_mask.bits())
        } else {
            mask
        };

        let mut safe = Vec::new();
        let mut unsafe_conc = Vec::new();

        if effective_mask.has(CapabilityMask::MANIFEST) {
            safe.push("can resolve symbol names and top-level declarations".into());
        } else {
            unsafe_conc.push("no symbol index available — cannot confirm any symbol exists".into());
        }

        if effective_mask.has(CapabilityMask::STRUCTURAL) {
            safe.push("can confirm all AST-level references and scope relationships".into());
        }

        if effective_mask.has(CapabilityMask::CALL_EDGES) {
            safe.push("can trace direct caller/callee relationships".into());
        } else {
            unsafe_conc
                .push("cannot confirm complete call graph — some calls may be missing".into());
        }

        if effective_mask.has(CapabilityMask::CFG) {
            safe.push("can analyze branch-level control flow".into());
        } else {
            unsafe_conc.push(
                "cannot analyze branch-level control flow — path-sensitive questions are speculative"
                    .into(),
            );
        }

        if effective_mask.has(CapabilityMask::DATAFLOW) {
            safe.push("can trace intra-procedural dataflow (def-use chains)".into());
        } else {
            unsafe_conc.push(
                "cannot confirm dataflow completeness — variable provenance may be incomplete"
                    .into(),
            );
        }

        if effective_mask.has(CapabilityMask::SUMMARIES) {
            safe.push("can trace inter-procedural dataflow via function summaries".into());
        } else {
            unsafe_conc.push(
                "cannot trace dataflow across function boundaries — argument/return flows are not verified"
                    .into(),
            );
        }

        let stats = capability_stats.unwrap_or_default();
        let summary = CapabilitySummary {
            mask_bits: effective_mask.bits(),
            best_capability: effective_mask.best_capability_name().into(),
            total_files: outcome
                .map(|o| o.files_built + o.files_cached + o.files_pending)
                .unwrap_or(0),
            files_with_dataflow: stats.files_with_dataflow,
            files_with_cfg: stats.files_with_cfg,
            files_structural_only: stats.files_structural_only,
            files_manifest_only: stats.files_manifest_only,
        };

        let mut jobs = Vec::new();
        if !effective_mask.has(CapabilityMask::CFG) {
            jobs.push(RefinementJob {
                description: "build CFG for functions in scope".into(),
                capability_needed: "cfg".into(),
            });
        }
        if !effective_mask.has(CapabilityMask::DATAFLOW) {
            jobs.push(RefinementJob {
                description: "build intra-procedural dataflow for functions in scope".into(),
                capability_needed: "dataflow".into(),
            });
        }

        Self {
            safe_conclusions: safe,
            unsafe_conclusions: unsafe_conc,
            capability_summary: summary,
            refinement_jobs: jobs,
        }
    }
}

impl LazyDiagnostics {
    /// Unified constructor accepting optional structural and dataflow outcomes.
    /// When both are None, returns None (caller should omit the diagnostics field).
    /// `capability_stats` is optional DB-sourced file counts; `None` defaults to zero.
    pub(crate) fn from_layers(
        structural_outcome: Option<&LazyOutcome>,
        dataflow_window: Option<&LazyWindow>,
        capability_stats: Option<CapabilityStats>,
    ) -> Option<Self> {
        if structural_outcome.is_none() && dataflow_window.is_none() {
            return None;
        }

        let structural = structural_outcome.map(|outcome| LazyLayerDiagnostics {
            triggered: true,
            files_built: outcome.files_built,
            files_cached: outcome.files_cached,
            files_pending: outcome.files_pending,
            budget_exceeded: outcome.budget_exceeded,
        });

        let dataflow = dataflow_window.map(|window| LazyLayerDiagnostics {
            triggered: true,
            files_built: window.units_built,
            files_cached: window.units_cached,
            files_pending: window.units_pending,
            budget_exceeded: window.truncated,
        });

        // Derive next_action from the combined state.
        let structural_pending = structural
            .as_ref()
            .is_some_and(|s| s.files_pending > 0);
        let dataflow_pending = dataflow
            .as_ref()
            .is_some_and(|d| d.files_pending > 0);

        let next_action = if structural_pending || dataflow_pending {
            "poll_jobs"
        } else if structural.as_ref().is_some_and(|s| s.budget_exceeded)
            || dataflow.as_ref().is_some_and(|d| d.budget_exceeded)
        {
            "narrow_scope"
        } else if structural.as_ref().is_some_and(|s| {
            s.files_built == 0
                && s.files_cached == 0
                && s.precision_tier() == PrecisionTier::Unavailable
        }) {
            "run_full_index"
        } else {
            "none"
        };

        // Compute analysis_contract from the combined capability mask.
        // Merge both structural and dataflow masks via bitwise OR so
        // the contract accurately reflects all built layers.
        let mut mask = CapabilityMask::default();
        if let Some(so) = structural_outcome {
            mask = CapabilityMask::new(mask.bits() | so.capability_mask.bits());
        }
        if let Some(dw) = dataflow_window {
            mask = CapabilityMask::new(mask.bits() | dw.capability_mask.bits());
        }

        Some(Self {
            structural,
            dataflow,
            next_action,
            analysis_contract: AnalysisContract::from_capability(
                mask,
                structural_outcome,
                capability_stats,
            ),
        })
    }

    /// Create diagnostics from a structural lazy extraction outcome.
    ///
    /// Thin wrapper around [`from_layers`].
    pub(crate) fn from_structural(outcome: &LazyOutcome) -> Self {
        Self::from_layers(Some(outcome), None, None)
            .expect("from_structural always has a structural outcome")
    }

    /// Create diagnostics from a structural outcome with real DB capability stats.
    ///
    /// Unlike [`from_structural`] (which injects `capability_stats=None`),
    /// this variant queries the DB for verified file counts so the
    /// `analysis_contract` reflects actual extraction state rather than
    /// theoretical capability.
    pub(crate) fn from_structural_with_stats(
        outcome: &LazyOutcome,
        stats: Option<&CapabilityStats>,
    ) -> Self {
        Self::from_layers(Some(outcome), None, stats.copied())
            .expect("from_structural_with_stats always has a structural outcome")
    }

    /// Create diagnostics from only a dataflow [`LazySummary`].
    /// Used when structural is already cached (no `LazyOutcome`) but the
    /// Engine triggered lazy dataflow loading.
    pub(crate) fn from_dataflow_summary(summary: &atlas_engine::LazySummary) -> Self {
        let dataflow = LazyLayerDiagnostics::from_lazy_summary(summary);
        let next_action = if dataflow.files_pending > 0 {
            "poll_jobs"
        } else if dataflow.budget_exceeded {
            "narrow_scope"
        } else {
            "none"
        };
        Self {
            structural: None,
            dataflow: Some(dataflow),
            next_action,
            // Dataflow extraction always provides at least manifest, structural,
            // call edges, and intra-procedural dataflow (but NOT CFG, which is
            // language-specific).
            analysis_contract: AnalysisContract::from_capability(
                CapabilityMask::new(
                    CapabilityMask::MANIFEST
                        | CapabilityMask::STRUCTURAL
                        | CapabilityMask::CALL_EDGES
                        | CapabilityMask::DATAFLOW,
                ),
                None,
                None,
            ),
        }
    }
}

impl LazyLayerDiagnostics {
    /// Build dataflow-layer diagnostics from Engine's [`LazySummary`].
    /// Used when the Engine handles lazy dataflow internally (P2#14 refactoring)
    /// and the MCP layer constructs combined diagnostics from structural +
    /// Engine-returned dataflow summary.
    pub(crate) fn from_lazy_summary(summary: &atlas_engine::LazySummary) -> Self {
        Self {
            triggered: summary.triggered,
            files_built: summary.units_built,
            files_cached: summary.units_cached,
            files_pending: summary.units_pending,
            budget_exceeded: summary.truncated,
        }
    }

    /// Approximate precision tier from layer diagnostics.
    fn precision_tier(&self) -> PrecisionTier {
        if self.files_built == 0 && self.files_cached == 0 {
            if self.budget_exceeded {
                PrecisionTier::ManifestOnly
            } else {
                PrecisionTier::Unavailable
            }
        } else if self.budget_exceeded {
            PrecisionTier::DegradedStructural
        } else {
            PrecisionTier::Exact
        }
    }
}

// ── SnapshotStore trait ─────────────────────────────────────────────────

/// Trait for storing query snapshots, implemented by [`super::ToolRouter`].
///
/// This decouples [`LazyResponse`] from the concrete router type so the
/// builder can store snapshots without knowing the handler's full context.
pub(crate) trait SnapshotStore {
    fn store_query_snapshot(&mut self, snapshot: QuerySnapshot);
}

// ── LazyResponse builder ──────────────────────────────────────────────

/// Envelope wrapper that adds common lazy-analysis fields to MCP tool
/// responses.
///
/// Eliminates the repeated pattern of generating `query_id`,
/// merging warnings, adding
/// `lazy_diagnostics`/`analysis_contract`, and storing a [`QuerySnapshot`].
///
/// # Usage
///
/// ```ignore
/// let lr = LazyResponse::new("trace", args)
///     .with_lazy_diag(lazy_diag)
///     .with_is_error(!resp.ok);
/// let (json_str, is_error) = lr.build(resp_value, self);
/// ```
pub(crate) struct LazyResponse {
    query_id: String,
    tool_name: String,
    tool_args: serde_json::Value,
    root_warnings: Vec<String>,
    lazy_warnings: Vec<String>,
    inject_analysis_contract: bool,
    status: Option<QueryStatus>,
    is_error_override: Option<bool>,
    partial_result: bool,
    /// Focus-aware precision (from new type system).
    precision: Option<Precision>,
    /// Distribution of results by coverage tier.
    coverage_counts: Option<HashMap<String, usize>>,
    /// Known gaps in analysis completeness.
    known_gaps: Option<Vec<KnownGap>>,
    /// Explicitly set analysis state (from focus path).
    analysis_state: Option<String>,
    /// Explicitly set analysis scope (from focus path).
    analysis_scope: Option<String>,
    /// Explicitly set analysis summary (from focus path).
    analysis_summary: Option<String>,
    /// Explicitly set analysis next action (from focus path).
    analysis_next_action: Option<String>,
    /// Explicitly set work items (from focus path).
    work_items: Option<Vec<WorkItem>>,
}

impl LazyResponse {
    /// Create a new `LazyResponse`, generating a fresh `query_id`.
    ///
    /// `tool_name` is stored in the [`QuerySnapshot`] for `atlas_resume`.
    /// `tool_args` is the original MCP arguments (cloned for storage).
    pub fn new(tool_name: &'static str, tool_args: &serde_json::Value) -> Self {
        Self {
            query_id: super::ToolRouter::generate_query_id(),
            tool_name: tool_name.to_string(),
            tool_args: tool_args.clone(),
            root_warnings: Vec::new(),
            lazy_warnings: Vec::new(),
            inject_analysis_contract: true,
            status: None,
            is_error_override: None,
            partial_result: false,
            precision: None,
            coverage_counts: None,
            known_gaps: None,
            analysis_state: None,
            analysis_scope: None,
            analysis_summary: None,
            analysis_next_action: None,
            work_items: None,
        }
    }

    /// Access the generated `query_id` (for passing to structural extraction).
    pub fn query_id(&self) -> &str {
        &self.query_id
    }

    /// Add root warnings (from the primary response, e.g. include_roots).
    pub fn with_root_warnings(mut self, warnings: Vec<String>) -> Self {
        self.root_warnings = warnings;
        self
    }

    /// Add lazy warnings (from lazy structural extraction).
    pub fn with_lazy_warnings(mut self, warnings: Vec<String>) -> Self {
        self.lazy_warnings = warnings;
        self
    }

    /// Whether to inject `analysis_contract`. Default is `true`.
    pub fn with_analysis_contract(mut self, inject: bool) -> Self {
        self.inject_analysis_contract = inject;
        self
    }

    /// Override the `is_error` flag in the returned tuple.
    /// When not set, `is_error` is derived from the body's `"ok"` or
    /// `"error"` field.
    pub fn with_is_error(mut self, is_error: bool) -> Self {
        self.is_error_override = Some(is_error);
        self
    }

    /// Mark that the result is partial (affects snapshot status).
    pub fn with_partial_result(mut self, partial: bool) -> Self {
        self.partial_result = partial;
        self
    }

    /// Set the new Precision (focus-aware).
    pub fn with_precision(mut self, precision: Precision) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Set coverage distribution counts.
    pub fn with_coverage_counts(mut self, counts: HashMap<String, usize>) -> Self {
        self.coverage_counts = Some(counts);
        self
    }

    /// Set known gaps.
    pub fn with_gaps(mut self, gaps: Vec<KnownGap>) -> Self {
        self.known_gaps = Some(gaps);
        self
    }

    /// Set analysis state (from focus path).
    pub fn with_analysis_state(mut self, state: String) -> Self {
        self.analysis_state = Some(state);
        self
    }

    /// Set analysis scope (from focus path).
    pub fn with_analysis_scope(mut self, scope: String) -> Self {
        self.analysis_scope = Some(scope);
        self
    }

    /// Set analysis summary (from focus path).
    pub fn with_analysis_summary(mut self, summary: String) -> Self {
        self.analysis_summary = Some(summary);
        self
    }

    /// Set analysis next action (from focus path).
    pub fn with_analysis_next_action(mut self, action: String) -> Self {
        self.analysis_next_action = Some(action);
        self
    }

    /// Set work items (from focus path).
    pub fn with_work_items(mut self, items: Vec<WorkItem>) -> Self {
        self.work_items = Some(items);
        self
    }

    /// Build the final response: merge envelope fields into `body`, store a
    /// [`QuerySnapshot`] (using `tool_args` for stored args), and return
    /// `(json_string, is_error)`.
    ///
    /// Envelope fields injected:
    /// - `warnings` (merged root + lazy, only when non-empty)
    /// - `query_id`
    /// - `analysis` block
    /// - `work` block (when work_items is Some)
    /// - `precision`, `coverage_counts`, `known_gaps` (when set)
    pub fn build(self, body: serde_json::Value, store: &mut impl SnapshotStore) -> (String, bool) {
        let args = self.tool_args.clone();
        self.build_with_args(body, &args, store)
    }

    /// Build the final response with custom stored args for the snapshot.
    ///
    /// Use this when the snapshot args differ from the original tool args
    /// (e.g., symbol detail adds `"view": "detail"`).
    pub fn build_with_args(
        self,
        mut body: serde_json::Value,
        stored_args: &serde_json::Value,
        store: &mut impl SnapshotStore,
    ) -> (String, bool) {
        // 1. Merge warnings into JSON "warnings" array (only when non-empty)
        let mut all_warnings = self.root_warnings;
        all_warnings.extend(self.lazy_warnings);
        if !all_warnings.is_empty() {
            body["warnings"] = serde_json::Value::Array(
                all_warnings
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            );
        }

        // 2. Inject query_id
        body["query_id"] = json!(self.query_id);

        // 3. Analysis block (only from explicitly-set fields, no fallback)
        let analysis_state = self.analysis_state.clone().unwrap_or_default();
        let analysis_scope = self.analysis_scope.clone().unwrap_or_default();
        let analysis_summary = self.analysis_summary.clone().unwrap_or_default();
        let analysis_next_action = self.analysis_next_action.clone().unwrap_or_default();

        body["analysis"] = json!({
            "state": analysis_state,
            "scope": analysis_scope,
            "summary": analysis_summary,
            "next_action": analysis_next_action,
        });

        // 4. Work block (only when relevant to this response)
        if let Some(ref items) = self.work_items {
            let status = if items.is_empty() { "idle" } else { "running" };
            body["work"] = json!({
                "relevant": true,
                "status": status,
                "items": items,
            });
        }

        // 5. Inject focus-aware precision (new type system)
        if let Some(ref p) = self.precision {
            body["precision"] = serde_json::to_value(p).unwrap_or(json!(null));
        }

        // 6. Inject coverage distribution counts
        if let Some(ref counts) = self.coverage_counts {
            body["coverage_counts"] = serde_json::to_value(counts).unwrap_or(json!({}));
        }

        // 7. Inject known gaps
        if let Some(ref gaps) = self.known_gaps {
            body["known_gaps"] = serde_json::to_value(gaps).unwrap_or(json!([]));
        }

        // 8. Store snapshot
        let status = self.status.unwrap_or(QueryStatus::Partial);
        store.store_query_snapshot(QuerySnapshot {
            query_id: self.query_id,
            tool_name: self.tool_name,
            tool_args: stored_args.clone(),
            lazy_window: None,
            created_at: std::time::Instant::now(),
            status,
        });

        // 9. Determine is_error
        let is_error = self.is_error_override.unwrap_or_else(|| {
            body.get("ok")
                .and_then(|v| v.as_bool())
                .map(|b| !b)
                .or_else(|| body.get("error").map(|_| true))
                .unwrap_or(false)
        });

        (
            serde_json::to_string_pretty(&body).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::structs::CapabilityMask;

    #[test]
    fn test_analysis_contract_from_manifest_only() {
        let mask = CapabilityMask::new(CapabilityMask::MANIFEST);
        let contract = AnalysisContract::from_capability(mask, None, None);

        // With manifest only, should report limited capabilities
        assert!(!contract.safe_conclusions.is_empty());
        // Should have refinement suggestions for structural
        assert!(!contract.refinement_jobs.is_empty());
    }

    #[test]
    fn test_analysis_contract_from_full_dataflow() {
        let mask = CapabilityMask::new(
            CapabilityMask::MANIFEST
                | CapabilityMask::STRUCTURAL
                | CapabilityMask::CALL_EDGES
                | CapabilityMask::CFG
                | CapabilityMask::DATAFLOW,
        );
        let contract = AnalysisContract::from_capability(mask, None, None);

        // Should report full analysis
        assert!(!contract.safe_conclusions.is_empty());
        // Should acknowledge dataflow capability
        let has_dataflow_conclusion = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("dataflow") || c.contains("Dataflow"));
        assert!(
            has_dataflow_conclusion,
            "Should mention dataflow in safe conclusions"
        );
    }

    #[test]
    fn test_analysis_contract_serialization() {
        let mask = CapabilityMask::new(CapabilityMask::STRUCTURAL);
        let contract = AnalysisContract::from_capability(mask, None, None);
        let json = serde_json::to_string(&contract).unwrap();
        assert!(json.contains("safe_conclusions"));
        assert!(json.contains("unsafe_conclusions"));
        assert!(json.contains("refinement_jobs"));
    }

    #[test]
    fn test_capability_summary_serialization() {
        let summary = CapabilitySummary {
            mask_bits: CapabilityMask::MANIFEST | CapabilityMask::STRUCTURAL,
            best_capability: "structural".into(),
            total_files: 10,
            files_with_dataflow: 0,
            files_with_cfg: 0,
            files_structural_only: 8,
            files_manifest_only: 2,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("structural"));
        assert!(json.contains("mask_bits"));
        assert!(json.contains("total_files"));
    }

    // ── AnalysisContract AND-reconciliation tests ─────────────────────

    #[test]
    fn contract_and_reconcile_dataflow_declared_but_no_files() {
        let mask = CapabilityMask::new(CapabilityMask::DATAFLOW);
        let stats = CapabilityStats {
            files_with_dataflow: 0,
            ..Default::default()
        };
        let contract = AnalysisContract::from_capability(mask, None, Some(stats));
        let has_dataflow = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("dataflow"));
        assert!(
            !has_dataflow,
            "dataflow should be AND-reconciled AWAY when no files have dataflow"
        );
    }

    #[test]
    fn contract_and_reconcile_dataflow_declared_with_files() {
        let mask = CapabilityMask::new(CapabilityMask::DATAFLOW);
        let stats = CapabilityStats {
            files_with_dataflow: 1,
            ..Default::default()
        };
        let contract = AnalysisContract::from_capability(mask, None, Some(stats));
        let has_dataflow = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("dataflow"));
        assert!(
            has_dataflow,
            "dataflow should be present in safe_conclusions when files have dataflow"
        );
    }

    #[test]
    fn contract_no_reconciliation_without_stats() {
        let mask = CapabilityMask::new(CapabilityMask::DATAFLOW);
        let contract = AnalysisContract::from_capability(mask, None, None);
        let has_dataflow = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("dataflow"));
        assert!(
            has_dataflow,
            "dataflow should be present when no stats are provided (backward compatible)"
        );
    }

    #[test]
    fn contract_and_reconcile_cfg() {
        let mask = CapabilityMask::new(CapabilityMask::CFG | CapabilityMask::STRUCTURAL);
        let stats = CapabilityStats {
            files_with_cfg: 0,
            files_structural_only: 1,
            ..Default::default()
        };
        let contract = AnalysisContract::from_capability(mask, None, Some(stats));

        // CFG should NOT be in safe_conclusions
        let has_cfg = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("branch-level control flow"));
        assert!(
            !has_cfg,
            "CFG should be reconciled AWAY when no files have CFG"
        );

        // Structural SHOULD be in safe_conclusions
        let has_structural = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("AST-level references"));
        assert!(
            has_structural,
            "structural should remain when files have structural extraction"
        );
    }

    #[test]
    fn contract_and_reconcile_manifest_only_structural_zero() {
        let mask =
            CapabilityMask::new(CapabilityMask::MANIFEST | CapabilityMask::STRUCTURAL);
        let stats = CapabilityStats {
            files_manifest_only: 1,
            files_structural_only: 0,
            ..Default::default()
        };
        let contract = AnalysisContract::from_capability(mask, None, Some(stats));

        // Manifest should be present (total > 0)
        let has_manifest = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("resolve symbol names"));
        assert!(has_manifest, "manifest should survive reconciliation");

        // Structural should be absent (files_structural_only=0, AND-reconciled away)
        let has_structural = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("AST-level references"));
        assert!(
            !has_structural,
            "structural should be downgraded when files_structural_only is 0"
        );
    }

    // ── CapabilityStats / capability_mask_from_counts tests ───────────

    #[test]
    fn capability_mask_from_counts_all_zero() {
        let stats = CapabilityStats::default();
        let mask = capability_mask_from_counts(&stats);
        assert!(
            mask.is_zero(),
            "all-zero stats should produce a zero mask"
        );
    }

    #[test]
    fn capability_mask_from_counts_dataflow_present() {
        let stats = CapabilityStats {
            files_with_dataflow: 5,
            ..Default::default()
        };
        let mask = capability_mask_from_counts(&stats);
        assert!(
            mask.has(CapabilityMask::DATAFLOW),
            "non-zero files_with_dataflow should set the DATAFLOW bit"
        );
        assert!(
            mask.has(CapabilityMask::SUMMARIES),
            "DATAFLOW should imply SUMMARIES"
        );
    }

    #[test]
    fn capability_mask_from_counts_cfg_present() {
        let stats = CapabilityStats {
            files_with_cfg: 3,
            ..Default::default()
        };
        let mask = capability_mask_from_counts(&stats);
        assert!(
            mask.has(CapabilityMask::CFG),
            "non-zero files_with_cfg should set the CFG bit"
        );
        assert!(
            mask.has(CapabilityMask::MANIFEST),
            "any file should set MANIFEST"
        );
    }

    // ── LazyDiagnostics layered test ──────────────────────────────────

    #[test]
    fn lazy_diagnostics_combined_structural_and_dataflow() {
        use atlas_engine::LazySummary;
        use atlas_engine::structs::precision::PrecisionTier;

        let structural_layer = LazyLayerDiagnostics {
            triggered: true,
            files_built: 5,
            files_cached: 2,
            files_pending: 0,
            budget_exceeded: false,
        };

        let summary = LazySummary {
            triggered: true,
            units_built: 1,
            units_cached: 0,
            units_pending: 0,
            pending_job_ids: vec![],
            truncated: false,
            duration_ms: 100,
            precision_tier: Some(PrecisionTier::Exact),
        };
        let dataflow_layer = LazyLayerDiagnostics::from_lazy_summary(&summary);

        let mask = CapabilityMask::new(
            CapabilityMask::MANIFEST
                | CapabilityMask::STRUCTURAL
                | CapabilityMask::CALL_EDGES
                | CapabilityMask::DATAFLOW,
        );
        let contract = AnalysisContract::from_capability(mask, None, None);

        let diag = LazyDiagnostics {
            structural: Some(structural_layer),
            dataflow: Some(dataflow_layer),
            next_action: "none",
            analysis_contract: contract,
        };

        let json = serde_json::to_string(&diag).unwrap();
        assert!(json.contains("structural"), "json should contain structural layer diagnostics");
        assert!(json.contains("dataflow"), "json should contain dataflow layer diagnostics");
        assert!(json.contains("next_action"), "json should contain next_action");
        assert!(json.contains("analysis_contract"), "json should contain analysis_contract");
    }

    // ── Shared test infrastructure ─────────────────────────────────────

    use crate::tools::query_snapshot::QuerySnapshot;

    struct MockStore {
        snapshots: Vec<QuerySnapshot>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                snapshots: Vec::new(),
            }
        }
    }

    impl super::SnapshotStore for MockStore {
        fn store_query_snapshot(&mut self, snapshot: QuerySnapshot) {
            self.snapshots.push(snapshot);
        }
    }

    // ── LazyResponse builder tests ────────────────────────────────────

    #[test]
    fn lazy_response_injects_query_id_and_diagnostics() {
        let args = json!({"symbol": "test_fn"});
        let lr = LazyResponse::new("test_tool", &args);
        let qid = lr.query_id().to_string();
        assert!(!qid.is_empty(), "query_id should be generated");

        let body = json!({"result": "ok"});
        let mut store = MockStore { snapshots: vec![] };
        let (json_str, is_err) = lr.with_is_error(false).build(body, &mut store);

        assert!(!is_err);
        assert!(json_str.contains("query_id"), "response must contain query_id");
        assert!(!store.snapshots.is_empty(), "snapshot must be stored");
        assert_eq!(store.snapshots[0].query_id, qid);
    }

    // ── Focus envelope tests ───────────────────────────────────────────

    #[test]
    fn test_lazy_response_with_precision() {
        let args = json!({"symbol": "test_fn"});
        let lr = LazyResponse::new("test_tool", &args)
            .with_precision(Precision {
                coverage: CoverageTier::ClosureComplete {
                    closure_id: "c1".into(),
                },
                confidence: SemanticConfidence::High,
            })
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &mut MockStore::new());

        assert!(!is_err);
        assert!(json_str.contains("\"precision\""), "should contain precision field");
        assert!(
            json_str.contains("ClosureComplete") || json_str.contains("\"c1\""),
            "should contain closure precision info"
        );
    }

    #[test]
    fn test_lazy_response_with_coverage_counts() {
        let args = json!({"symbol": "test_fn"});
        let mut counts = HashMap::new();
        counts.insert("repo_complete".to_string(), 12usize);
        counts.insert("partial".to_string(), 3usize);

        let lr = LazyResponse::new("test_tool", &args)
            .with_coverage_counts(counts)
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &mut MockStore::new());

        assert!(!is_err);
        assert!(
            json_str.contains("coverage_counts"),
            "should contain coverage_counts field"
        );
        assert!(
            json_str.contains("repo_complete"),
            "should contain repo_complete key"
        );
    }

    #[test]
    fn test_lazy_response_with_gaps() {
        let args = json!({"symbol": "test_fn"});
        let gaps = vec![KnownGap::UnresolvedImport {
            from: "foo.c".into(),
            import_path: "bar.h".into(),
        }];

        let lr = LazyResponse::new("test_tool", &args)
            .with_gaps(gaps)
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &mut MockStore::new());

        assert!(!is_err);
        assert!(json_str.contains("known_gaps"), "should contain known_gaps field");
        assert!(
            json_str.contains("UnresolvedImport"),
            "should contain the gap variant"
        );
    }

    #[test]
    fn test_lazy_response_emits_analysis_block() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = LazyResponse::new("explore", &args)
            .with_is_error(false);

        let body = json!({"ok": true, "data": "test_result"});
        let (json_str, is_err) = lr.build(body, &mut store);
        assert!(!is_err);
        assert!(json_str.contains("\"analysis\""));
        assert!(json_str.contains("\"state\""));
        assert!(json_str.contains("\"scope\""));
        assert!(json_str.contains("\"summary\""));
        assert!(json_str.contains("\"next_action\""));
    }

    #[test]
    fn test_lazy_response_no_work_block_without_explicit_work_items() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = LazyResponse::new("explore", &args);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &mut store);
        // Work block should NOT be emitted when no work_items are explicitly set
        assert!(!json_str.contains("\"work\""));
    }

    #[test]
    fn test_lazy_response_explicit_analysis_fields() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = LazyResponse::new("explore", &args)
            .with_analysis_state("ready".into())
            .with_analysis_scope("local".into())
            .with_analysis_summary("custom summary".into())
            .with_analysis_next_action("use_result".into())
            .with_is_error(false);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &mut store);
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["analysis"]["state"], "ready");
        assert_eq!(v["analysis"]["summary"], "custom summary");
    }

    #[test]
    fn test_lazy_response_explicit_work_items() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = LazyResponse::new("explore", &args)
            .with_work_items(vec![WorkItem {
                id: "job-focus".into(),
                kind: "extraction".into(),
                state: "building".into(),
                scope: "local".into(),
                reason: "focus".into(),
                progress: Some(WorkProgress { percent: 75 }),
                waitable: true,
                retry_after_ms: Some(1000),
            }]);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &mut store);
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["work"]["relevant"], true);
        assert_eq!(v["work"]["status"], "running");
        assert_eq!(v["work"]["items"][0]["id"], "job-focus");
    }

    #[test]
    fn test_full_response_envelope() {
        let args = json!({"symbol": "test_fn"});
        let mut counts = HashMap::new();
        counts.insert("repo_complete".to_string(), 5usize);

        let lr = LazyResponse::new("test_tool", &args)
            .with_precision(Precision {
                coverage: CoverageTier::RepoComplete,
                confidence: SemanticConfidence::Certain,
            })
            .with_coverage_counts(counts)
            .with_gaps(vec![KnownGap::UnresolvedImport {
                from: "a.c".into(),
                import_path: "b.h".into(),
            }])
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &mut MockStore::new());

        assert!(!is_err);
        assert!(json_str.contains("precision"), "should contain precision");
        assert!(json_str.contains("coverage_counts"), "should contain coverage_counts");
        assert!(json_str.contains("known_gaps"), "should contain known_gaps");
        assert!(json_str.contains("query_id"), "should contain query_id");
    }

    #[test]
    fn test_coverage_counts_empty() {
        let args = json!({"symbol": "test_fn"});
        let counts: HashMap<String, usize> = HashMap::new();

        let lr = LazyResponse::new("test_tool", &args)
            .with_coverage_counts(counts)
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &mut MockStore::new());

        assert!(!is_err);
        assert!(
            json_str.contains("coverage_counts"),
            "should contain coverage_counts even when empty"
        );
    }
}
