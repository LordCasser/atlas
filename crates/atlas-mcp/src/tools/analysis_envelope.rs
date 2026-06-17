//! Response envelope for MCP tool responses.
//!
//! Provides [`AnalysisEnvelope`] — a builder that centralizes the common
//! response envelope pattern shared by "full envelope" tool handlers:
//! generating a `query_id`, merging warnings, and storing a
//! [`super::query_snapshot::QuerySnapshot`].

use std::collections::HashMap;

#[cfg(test)]
use atlas_engine::structs::CoverageTier;
use atlas_engine::structs::KnownGap;
use atlas_engine::structs::Precision;
#[cfg(test)]
use atlas_engine::structs::SemanticConfidence;
use serde::Serialize;
use serde_json::json;

use super::analysis_response::precision_to_view;
use super::query_snapshot::{QuerySnapshot, QueryStatus};

/// Optional project-wide capability statistics populated from the DB.
/// When `None`, all counts default to 0 (the caller hasn't queried yet).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CapabilityStats {
    pub files_with_dataflow: usize,
    pub files_structural_only: usize,
    pub files_manifest_only: usize,
    /// Number of files with CFG analysis.
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

// ── SnapshotStore trait ─────────────────────────────────────────────────

/// Trait for storing query snapshots, implemented by [`super::ToolRouter`].
///
/// This decouples [`AnalysisEnvelope`] from the concrete router type so the
/// builder can store snapshots without knowing the handler's full context.
pub(crate) trait SnapshotStore {
    fn store_query_snapshot(&self, snapshot: QuerySnapshot);
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
    /// Explicitly set analysis scope (from focus path).
    analysis_scope: Option<String>,
    /// Explicitly set analysis summary (from focus path).
    analysis_summary: Option<String>,
    /// Local analysis unit this response covers, e.g. "function".
    analysis_unit: Option<String>,
    /// Public coverage label for the analysis unit.
    analysis_coverage: Option<String>,
    /// Capabilities or facts this response actually used.
    analysis_basis: Option<Vec<String>>,
    /// Capabilities or facts missing from a stronger answer.
    analysis_missing: Option<Vec<String>>,
    /// Suggested delay before retrying/resuming this query.
    analysis_retry_after_ms: Option<u64>,
    /// Structured gap records for the response envelope (MCP v2 format).
    gap_records: Option<Vec<GapRecord>>,
    /// Explicit background refinement status for partial focus responses.
    background_refinement: Option<BackgroundRefinement>,
    /// Project-level index statistics for non-focus full-index responses.
    capability_stats: Option<CapabilityStats>,
    /// Project-level index snapshot (file/symbol/edge counts, index mode).
    project_stats: Option<ProjectStats>,
}

#[derive(Debug, Clone)]
struct BackgroundRefinement {
    state: String,
    job_count: Option<usize>,
    retry_after_ms: u64,
    description: String,
}

/// A known gap in the current analysis — what is missing and why.
/// Serialized as `{"gaps": [...]}` in the MCP response.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GapRecord {
    /// Scope of the gap — function qualified name or file path.
    pub scope: String,
    /// Machine-readable reason code (no_dataflow, no_cfg, no_transitions, etc.).
    pub reason: String,
    /// Human-readable detail describing what the agent should know.
    pub detail: String,
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
            analysis_scope: None,
            analysis_summary: None,
            analysis_unit: None,
            analysis_coverage: None,
            analysis_basis: None,
            analysis_missing: None,
            analysis_retry_after_ms: None,
            gap_records: None,
            background_refinement: None,
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

    /// Set the local unit this analysis response covers.
    pub fn with_analysis_unit(mut self, unit: String) -> Self {
        self.analysis_unit = Some(unit);
        self
    }

    /// Set the coverage label for the analysis unit.
    pub fn with_analysis_coverage(mut self, coverage: String) -> Self {
        self.analysis_coverage = Some(coverage);
        self
    }

    /// Set the facts/capabilities used for this response.
    pub fn with_analysis_basis(mut self, basis: Vec<String>) -> Self {
        self.analysis_basis = Some(basis);
        self
    }

    /// Set the facts/capabilities missing from a stronger response.
    pub fn with_analysis_missing(mut self, missing: Vec<String>) -> Self {
        self.analysis_missing = Some(missing);
        self
    }

    /// Set a suggested delay before retrying/resuming this query.
    pub fn with_analysis_retry_after_ms(mut self, retry_after_ms: u64) -> Self {
        self.analysis_retry_after_ms = Some(retry_after_ms);
        self
    }

    /// Set structured gap records (MCP v2 format).
    /// Replaces `with_analysis_missing` for tools that provide structured gaps.
    pub fn with_gap_records(mut self, gaps: Vec<GapRecord>) -> Self {
        self.gap_records = Some(gaps);
        self
    }

    /// Set explicit background refinement metadata.
    pub fn with_background_refinement(
        mut self,
        state: impl Into<String>,
        job_count: Option<usize>,
        retry_after_ms: u64,
        description: impl Into<String>,
    ) -> Self {
        self.background_refinement = Some(BackgroundRefinement {
            state: state.into(),
            job_count,
            retry_after_ms,
            description: description.into(),
        });
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
    /// - `precision`, `coverage_counts`, `gaps` (when set)
    pub fn build(self, body: serde_json::Value, store: &impl SnapshotStore) -> (String, bool) {
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
        store: &impl SnapshotStore,
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
        //    When focus analysis scope/summary is set, use focus fields.
        //    Otherwise compute from project stats / capability stats.
        if self.analysis_scope.is_some() || self.analysis_summary.is_some() {
            let analysis_scope = self.analysis_scope.clone().unwrap_or_default();
            let analysis_summary = self.analysis_summary.clone().unwrap_or_default();

            let mut analysis = json!({
                "scope": analysis_scope,
                "summary": analysis_summary,
            });
            if let Some(ref basis) = self.analysis_basis {
                analysis["basis"] = serde_json::to_value(basis).unwrap_or(json!([]));
            }
            if let Some(ref unit) = self.analysis_unit {
                analysis["unit"] = json!(unit);
            }
            if let Some(ref coverage) = self.analysis_coverage {
                analysis["coverage"] = json!(coverage);
            }
            if let Some(ref missing) = self.analysis_missing {
                analysis["missing"] = serde_json::to_value(missing).unwrap_or(json!([]));
            }
            if let Some(retry_after_ms) = self.analysis_retry_after_ms {
                analysis["retry_after_ms"] = json!(retry_after_ms);
            }
            body["analysis"] = analysis;
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
                    "scope": "repo",
                    "summary": summary,
                });
            }
        }

        if self.partial_result {
            body["partial_result"] = json!(true);
            if body.get("background_refinement").is_none() {
                if let Some(ref refinement) = self.background_refinement {
                    body["background_refinement"] = json!({
                        "state": refinement.state,
                        "job_count": refinement.job_count,
                        "retry_after_ms": refinement.retry_after_ms,
                        "description": refinement.description
                    });
                } else if let Some(retry_after_ms) = self.analysis_retry_after_ms {
                    body["background_refinement"] = json!({
                        "state": "pending",
                        "job_count": null,
                        "retry_after_ms": retry_after_ms,
                        "description": "background focus refinement is continuing for this partial result"
                    });
                }
            }
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

        // 7b. Inject structured gap records (MCP v2 format)
        if let Some(ref records) = self.gap_records {
            if !records.is_empty() {
                body["gaps"] = serde_json::to_value(records).unwrap_or(json!([]));
            }
        }

        // 8. Store snapshot
        let status = self.status.unwrap_or(if self.partial_result {
            QueryStatus::Partial
        } else {
            QueryStatus::Ready
        });
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

    // ── Shared test infrastructure ─────────────────────────────────────

    use crate::tools::query_snapshot::QuerySnapshot;
    use std::sync::Mutex;

    struct MockStore {
        snapshots: Mutex<Vec<QuerySnapshot>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                snapshots: Mutex::new(Vec::new()),
            }
        }
    }

    impl super::SnapshotStore for MockStore {
        fn store_query_snapshot(&self, snapshot: QuerySnapshot) {
            self.snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(snapshot);
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
        let store = MockStore::new();
        let (json_str, is_err) = lr.with_is_error(false).build(body, &store);

        assert!(!is_err);
        assert!(
            json_str.contains("query_id"),
            "response must contain query_id"
        );
        let snapshots = store.snapshots.lock().unwrap();
        assert!(!snapshots.is_empty(), "snapshot must be stored");
        assert_eq!(snapshots[0].query_id, qid);
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
        let (json_str, is_err) = lr.build(body, &MockStore::new());

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
        let (json_str, is_err) = lr.build(body, &MockStore::new());

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
        let (json_str, is_err) = lr.build(body, &MockStore::new());

        assert!(!is_err);
        assert!(json_str.contains("\"gaps\""), "should contain gaps field");
        assert!(
            json_str.contains("UnresolvedImport"),
            "should contain the gap variant"
        );
    }

    #[test]
    fn test_lazy_response_no_analysis_block_without_explicit_data() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args).with_is_error(false);

        let body = json!({"ok": true, "data": "test_result"});
        let (json_str, is_err) = lr.build(body, &store);
        assert!(!is_err);
        // Analysis block should NOT be emitted when no analysis fields are explicitly set
        assert!(!json_str.contains("\"analysis\""));
    }

    #[test]
    fn test_lazy_response_explicit_analysis_data_emits_block() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args)
            .with_is_error(false)
            .with_analysis_scope("local".into());

        let body = json!({"ok": true, "data": "test_result"});
        let (json_str, is_err) = lr.build(body, &store);
        assert!(!is_err);
        // Analysis block should be emitted when analysis_scope is explicitly set
        assert!(json_str.contains("\"analysis\""));
        assert!(json_str.contains("\"scope\""));
    }

    #[test]
    fn test_lazy_response_never_emits_work_block() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args)
            .with_analysis_scope("local".into())
            .with_analysis_summary("building analysis".into())
            .with_analysis_retry_after_ms(2000);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &store);
        assert!(!json_str.contains("\"work\""));
    }

    #[test]
    fn test_partial_response_emits_explicit_background_refinement() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("calls", &args)
            .with_partial_result(true)
            .with_analysis_scope("local".into())
            .with_analysis_summary("building analysis".into())
            .with_analysis_retry_after_ms(2000)
            .with_background_refinement(
                "queued",
                Some(3),
                2000,
                "background focus refinement is continuing",
            )
            .with_is_error(false);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &store);
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["background_refinement"]["state"], "queued");
        assert_eq!(v["background_refinement"]["job_count"], 3);
        assert_eq!(v["background_refinement"]["retry_after_ms"], 2000);
    }

    #[test]
    fn test_lazy_response_explicit_analysis_fields() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args)
            .with_analysis_scope("local".into())
            .with_analysis_summary("custom summary".into())
            .with_analysis_unit("function".into())
            .with_analysis_coverage("function_complete".into())
            .with_analysis_basis(vec!["cfg".into()])
            .with_analysis_missing(vec!["dataflow".into()])
            .with_analysis_retry_after_ms(2000)
            .with_is_error(false);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &store);
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["analysis"]["scope"], "local");
        assert_eq!(v["analysis"]["summary"], "custom summary");
        assert_eq!(v["analysis"]["unit"], "function");
        assert_eq!(v["analysis"]["coverage"], "function_complete");
        assert_eq!(v["analysis"]["basis"], json!(["cfg"]));
        assert_eq!(v["analysis"]["missing"], json!(["dataflow"]));
        assert_eq!(v["analysis"]["retry_after_ms"], 2000);
        assert!(v.get("work").is_none());
        // state and next_action must not be present
        assert!(v["analysis"].get("state").is_none(), "state field must be absent");
        assert!(v["analysis"].get("next_action").is_none(), "next_action field must be absent");
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
        let (json_str, is_err) = lr.build(body, &MockStore::new());

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
        let (json_str, is_err) = lr.build(body, &MockStore::new());

        assert!(!is_err);
        assert!(
            json_str.contains("coverage_counts"),
            "should contain coverage_counts even when empty"
        );
    }

    #[test]
    fn test_gap_records_serialize_as_structured_gaps() {
        let args = json!({"symbol": "test_fn"});
        let gap = GapRecord {
            scope: "function foo".into(),
            reason: "no_dataflow".into(),
            detail: "Dataflow not available".into(),
        };

        let lr = AnalysisEnvelope::new("test_tool", &args)
            .with_gap_records(vec![gap])
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &MockStore::new());

        assert!(!is_err);
        assert!(
            json_str.contains("\"gaps\""),
            "should contain gaps key"
        );
        // Parse and verify structure
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let gaps = v["gaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0]["scope"], "function foo");
        assert_eq!(gaps[0]["reason"], "no_dataflow");
        assert_eq!(gaps[0]["detail"], "Dataflow not available");
        // Must NOT contain background_refinement
        assert!(
            !json_str.contains("background_refinement"),
            "should not contain background_refinement"
        );
    }

    #[test]
    fn test_empty_gap_records_do_not_appear() {
        let args = json!({"symbol": "test_fn"});

        let lr = AnalysisEnvelope::new("test_tool", &args)
            .with_gap_records(vec![])
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &MockStore::new());

        assert!(!is_err);
        assert!(
            !json_str.contains("\"gaps\""),
            "empty gaps should not appear in response"
        );
    }
}
