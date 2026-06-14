//! Response envelope and analysis contract for MCP tool responses.
//!
//! Provides [`AnalysisEnvelope`] — a builder that centralizes the common
//! response envelope pattern shared by "full envelope" tool handlers:
//! generating a `query_id`, merging warnings, and storing a
//! [`super::query_snapshot::QuerySnapshot`].

use std::collections::HashMap;

#[cfg(test)]
use atlas_engine::structs::CapabilityMask;
#[cfg(test)]
use atlas_engine::structs::CoverageTier;
use atlas_engine::structs::KnownGap;
use atlas_engine::structs::Precision;
#[cfg(test)]
use atlas_engine::structs::SemanticConfidence;
#[cfg(test)]
use serde::Serialize;
use serde_json::json;

#[cfg(test)]
use super::analysis_response::WorkProgress;
use super::analysis_response::{WorkItem, precision_to_view};
use super::query_snapshot::{QuerySnapshot, QueryStatus};

// ── Analysis Contract ───────────────────────────────────────────────────

/// Analysis contract: what conclusions are safe/unsafe given current extraction state.
#[cfg(test)]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalysisContract {
    pub safe_conclusions: Vec<String>,
    pub unsafe_conclusions: Vec<String>,
    pub capability_summary: CapabilitySummary,
    pub refinement_jobs: Vec<RefinementJob>,
}

/// Summary of capability masks available across the project.
#[cfg(test)]
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
    /// Number of files with CFG analysis (reserved for future analysis contract).
    #[allow(dead_code)]
    pub files_with_cfg: usize,
}

/// Snapshot of project-level index statistics for the analysis envelope.
/// Carries index tier, file/symbol/edge counts available from the project.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectStats {
    /// Index analysis mode: "manifest", "structural", or "full".
    pub index_mode: Option<String>,
    pub total_files: usize,
    pub total_symbols: usize,
    pub total_edges: usize,
}

/// Derive an actual capability mask from DB-sourced file counts.
/// Only sets bits that have at least one verified file.
#[cfg(test)]
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
#[cfg(test)]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RefinementJob {
    pub description: String,
    pub capability_needed: String,
}

#[cfg(test)]
impl AnalysisContract {
    /// Build an AnalysisContract from a capability mask.
    /// `capability_stats` populates file-count breakdowns; pass `None` for zero defaults.
    ///
    /// When `capability_stats` is provided, the theoretical `mask` is AND‑reconciled
    /// with the actual DB file counts so that a bit is only claimed if at least one
    /// file has been verified at that tier.
    pub(crate) fn from_capability(
        mask: CapabilityMask,
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
            total_files: stats.files_with_dataflow
                + stats.files_with_cfg
                + stats.files_structural_only
                + stats.files_manifest_only,
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

// ── SnapshotStore trait ─────────────────────────────────────────────────

/// Trait for storing query snapshots, implemented by [`super::ToolRouter`].
///
/// This decouples [`AnalysisEnvelope`] from the concrete router type so the
/// builder can store snapshots without knowing the handler's full context.
pub(crate) trait SnapshotStore {
    fn store_query_snapshot(&mut self, snapshot: QuerySnapshot);
}

// ── AnalysisEnvelope builder ──────────────────────────────────────────────

/// Envelope wrapper that adds common lazy-analysis fields to MCP tool
/// responses.
///
/// Eliminates the repeated pattern of generating `query_id`,
/// merging warnings, and storing a [`QuerySnapshot`].
pub(crate) struct AnalysisEnvelope {
    query_id: String,
    tool_name: String,
    tool_args: serde_json::Value,
    root_warnings: Vec<String>,
    lazy_warnings: Vec<String>,
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
    /// Project-level index statistics for non-focus full-index responses.
    capability_stats: Option<CapabilityStats>,
    /// Project-level index snapshot (file/symbol/edge counts, index mode).
    project_stats: Option<ProjectStats>,
}

impl AnalysisEnvelope {
    /// Create a new `AnalysisEnvelope`, generating a fresh `query_id`.
    ///
    /// `tool_name` is stored in the [`QuerySnapshot`] for `resume_query`.
    /// `tool_args` is the original MCP arguments (cloned for storage).
    pub fn new(tool_name: &'static str, tool_args: &serde_json::Value) -> Self {
        Self {
            query_id: super::ToolRouter::generate_query_id(),
            tool_name: tool_name.to_string(),
            tool_args: tool_args.clone(),
            root_warnings: Vec::new(),
            lazy_warnings: Vec::new(),
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
            capability_stats: None,
            project_stats: None,
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

    /// Set capability statistics (for non-focus full-index analysis block).
    #[allow(dead_code)]
    pub fn with_capability_stats(mut self, stats: CapabilityStats) -> Self {
        self.capability_stats = Some(stats);
        self
    }

    /// Set project-level index snapshot (file/symbol/edge counts, index mode).
    #[allow(dead_code)]
    pub fn with_project_stats(mut self, stats: ProjectStats) -> Self {
        self.project_stats = Some(stats);
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

        // 3. Analysis block — always present.
        //    When focus analysis state is set, use focus fields.
        //    Otherwise compute from project stats / capability stats.
        if self.analysis_state.is_some() {
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
        } else {
            // Non-focus: compute analysis block from project / capability stats.
            let ps = self.project_stats.as_ref();
            let cs = self.capability_stats.as_ref();
            let have_any = ps.is_some() || cs.is_some();

            if have_any {
                let index_mode = ps
                    .and_then(|p| p.index_mode.as_deref())
                    .unwrap_or("unknown");
                let total_files = ps.map(|p| p.total_files).unwrap_or(0usize);
                let total_symbols = ps.map(|p| p.total_symbols).unwrap_or(0usize);
                let total_edges = ps.map(|p| p.total_edges).unwrap_or(0usize);

                let dataflow = cs.map(|c| c.files_with_dataflow).unwrap_or(0);
                let structural = cs.map(|c| c.files_structural_only).unwrap_or(0);
                let manifest = cs.map(|c| c.files_manifest_only).unwrap_or(0);

                let summary = format!(
                    "Indexed ({index_mode}): {total_files} files ({dataflow} dataflow, {structural} structural, {manifest} manifest), {total_symbols} symbols, {total_edges} edges"
                );

                body["analysis"] = json!({
                    "state": "ready",
                    "scope": "repo",
                    "summary": summary,
                    "next_action": "use_result",
                });
            }
        }

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
            let view = precision_to_view(p);
            body["precision"] = serde_json::to_value(view).unwrap_or(json!(null));
        }

        // 6. Inject coverage distribution counts
        if let Some(ref counts) = self.coverage_counts {
            body["coverage_counts"] = serde_json::to_value(counts).unwrap_or(json!({}));
        }

        // 7. Inject known gaps
        if let Some(ref gaps) = self.known_gaps {
            body["gaps"] = serde_json::to_value(gaps).unwrap_or(json!([]));
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
        let contract = AnalysisContract::from_capability(mask, None);

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
        let contract = AnalysisContract::from_capability(mask, None);

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
        let contract = AnalysisContract::from_capability(mask, None);
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
        let contract = AnalysisContract::from_capability(mask, Some(stats));
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
        let contract = AnalysisContract::from_capability(mask, Some(stats));
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
        let contract = AnalysisContract::from_capability(mask, None);
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
        let contract = AnalysisContract::from_capability(mask, Some(stats));

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
        let mask = CapabilityMask::new(CapabilityMask::MANIFEST | CapabilityMask::STRUCTURAL);
        let stats = CapabilityStats {
            files_manifest_only: 1,
            files_structural_only: 0,
            ..Default::default()
        };
        let contract = AnalysisContract::from_capability(mask, Some(stats));

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
        assert!(mask.is_zero(), "all-zero stats should produce a zero mask");
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

    // ── AnalysisEnvelope builder tests ────────────────────────────────────

    #[test]
    fn lazy_response_injects_query_id_and_diagnostics() {
        let args = json!({"symbol": "test_fn"});
        let lr = AnalysisEnvelope::new("test_tool", &args);
        let qid = lr.query_id().to_string();
        assert!(!qid.is_empty(), "query_id should be generated");

        let body = json!({"result": "ok"});
        let mut store = MockStore { snapshots: vec![] };
        let (json_str, is_err) = lr.with_is_error(false).build(body, &mut store);

        assert!(!is_err);
        assert!(
            json_str.contains("query_id"),
            "response must contain query_id"
        );
        assert!(!store.snapshots.is_empty(), "snapshot must be stored");
        assert_eq!(store.snapshots[0].query_id, qid);
    }

    // ── Focus envelope tests ───────────────────────────────────────────

    #[test]
    fn test_lazy_response_with_precision() {
        let args = json!({"symbol": "test_fn"});
        let lr = AnalysisEnvelope::new("test_tool", &args)
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
        assert!(
            json_str.contains("\"precision\""),
            "should contain precision field"
        );
        // PrecisionView uses public labels: coverage=local_complete, confidence=high
        assert!(
            json_str.contains("local_complete"),
            "ClosureComplete should map to public label 'local_complete'"
        );
        assert!(
            json_str.contains("high"),
            "confidence should be serialized as 'high'"
        );
        // closure_id MUST NOT leak
        assert!(
            !json_str.contains("\"closure_id\""),
            "closure_id must not leak into MCP response"
        );
        assert!(
            !json_str.contains("\"c1\""),
            "internal closure_id value must not leak"
        );
    }

    #[test]
    fn test_lazy_response_with_coverage_counts() {
        let args = json!({"symbol": "test_fn"});
        let mut counts = HashMap::new();
        counts.insert("repo_complete".to_string(), 12usize);
        counts.insert("partial".to_string(), 3usize);

        let lr = AnalysisEnvelope::new("test_tool", &args)
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

        let lr = AnalysisEnvelope::new("test_tool", &args)
            .with_gaps(gaps)
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &mut MockStore::new());

        assert!(!is_err);
        assert!(json_str.contains("\"gaps\""), "should contain gaps field");
        assert!(
            json_str.contains("UnresolvedImport"),
            "should contain the gap variant"
        );
    }

    #[test]
    fn test_lazy_response_no_analysis_block_without_explicit_data() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args).with_is_error(false);

        let body = json!({"ok": true, "data": "test_result"});
        let (json_str, is_err) = lr.build(body, &mut store);
        assert!(!is_err);
        // Analysis block should NOT be emitted when no analysis fields are explicitly set
        assert!(!json_str.contains("\"analysis\""));
    }

    #[test]
    fn test_lazy_response_explicit_analysis_data_emits_block() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args)
            .with_is_error(false)
            .with_analysis_state("building".into());

        let body = json!({"ok": true, "data": "test_result"});
        let (json_str, is_err) = lr.build(body, &mut store);
        assert!(!is_err);
        // Analysis block should be emitted when analysis_state is explicitly set
        assert!(json_str.contains("\"analysis\""));
        assert!(json_str.contains("\"state\""));
    }

    #[test]
    fn test_lazy_response_no_work_block_without_explicit_work_items() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &mut store);
        // Work block should NOT be emitted when no work_items are explicitly set
        assert!(!json_str.contains("\"work\""));
    }

    #[test]
    fn test_lazy_response_explicit_analysis_fields() {
        let mut store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args)
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
        let lr = AnalysisEnvelope::new("explore", &args).with_work_items(vec![WorkItem {
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

        let lr = AnalysisEnvelope::new("test_tool", &args)
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
        assert!(
            json_str.contains("coverage_counts"),
            "should contain coverage_counts"
        );
        assert!(json_str.contains("gaps"), "should contain gaps");
        assert!(json_str.contains("query_id"), "should contain query_id");
    }

    #[test]
    fn test_coverage_counts_empty() {
        let args = json!({"symbol": "test_fn"});
        let counts: HashMap<String, usize> = HashMap::new();

        let lr = AnalysisEnvelope::new("test_tool", &args)
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
