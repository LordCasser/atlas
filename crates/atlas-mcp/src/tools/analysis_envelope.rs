//! Response envelope for MCP tool responses.
//!
//! Provides [`AnalysisEnvelope`] — a builder that centralizes the common
//! response envelope pattern shared by "full envelope" tool handlers:
//! generating a `query_id`, merging warnings, and storing a
//! [`super::query_snapshot::QuerySnapshot`].

use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;

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
    is_error_override: Option<bool>,
    /// Distribution of results by coverage tier.
    coverage_counts: Option<HashMap<String, usize>>,
    /// Explicitly set analysis scope (from focus path).
    analysis_scope: Option<String>,
    /// Explicitly set analysis summary (from focus path).
    analysis_summary: Option<String>,
    /// Capabilities or facts this response actually used.
    analysis_basis: Option<Vec<String>>,
    /// Suggested delay before retrying/resuming this query.
    analysis_retry_after_ms: Option<u64>,
    /// Structured gap records for the response envelope (MCP v2 format).
    gap_records: Option<Vec<GapRecord>>,
    /// Original focus state retained for terminal-aware resume replay.
    focus_result: Option<atlas_engine::focus::runtime::FocusResult>,
    /// Project-level index statistics for non-focus full-index responses.
    capability_stats: Option<CapabilityStats>,
    /// Project-level index snapshot (file/symbol/edge counts, index mode).
    project_stats: Option<ProjectStats>,
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
            is_error_override: None,
            coverage_counts: None,
            analysis_scope: None,
            analysis_summary: None,
            analysis_basis: None,
            analysis_retry_after_ms: None,
            gap_records: None,
            focus_result: None,
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

    /// Set coverage distribution counts.
    pub fn with_coverage_counts(mut self, counts: HashMap<String, usize>) -> Self {
        self.coverage_counts = Some(counts);
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

    /// Set the facts/capabilities used for this response.
    pub fn with_analysis_basis(mut self, basis: Vec<String>) -> Self {
        self.analysis_basis = Some(basis);
        self
    }

    /// Set a suggested delay before retrying/resuming this query.
    pub fn with_analysis_retry_after_ms(mut self, retry_after_ms: u64) -> Self {
        self.analysis_retry_after_ms = Some(retry_after_ms);
        self
    }

    pub fn with_focus_result(mut self, result: atlas_engine::focus::runtime::FocusResult) -> Self {
        self.focus_result = Some(result);
        self
    }

    /// Set structured gap records.
    pub fn with_gap_records(mut self, gaps: Vec<GapRecord>) -> Self {
        self.gap_records = Some(gaps);
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
    /// - `coverage_counts`, `gaps` (when set)
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

        // 5. Inject coverage distribution counts
        if let Some(ref counts) = self.coverage_counts {
            body["coverage_counts"] = serde_json::to_value(counts).unwrap_or(json!({}));
        }

        // Gaps describe permanent terminal limitations. While retry guidance
        // is present, pending work can still change the result.
        if self.analysis_retry_after_ms.is_none() {
            if let Some(ref records) = self.gap_records {
                if !records.is_empty() {
                    body["gaps"] = serde_json::to_value(records).unwrap_or(json!([]));
                }
            }
        }

        // 8. Store snapshot
        let status = if self.analysis_retry_after_ms.is_some() {
            QueryStatus::Retryable
        } else {
            QueryStatus::Ready
        };
        store.store_query_snapshot(QuerySnapshot {
            query_id: self.query_id,
            tool_name: self.tool_name,
            tool_args: stored_args.clone(),
            focus_result: self.focus_result.clone(),
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
        let store = MockStore::new();
        let args = json!({"symbol": "test_fn"});
        let gaps = vec![GapRecord {
            scope: "foo.c".into(),
            reason: "unresolved_import".into(),
            detail: "Import 'bar.h' could not be resolved from this file.".into(),
        }];

        let lr = AnalysisEnvelope::new("test_tool", &args)
            .with_gap_records(gaps)
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &store);

        assert!(!is_err);
        assert!(json_str.contains("\"gaps\""), "should contain gaps field");
        assert!(
            json_str.contains("unresolved_import"),
            "should contain the stable gap reason"
        );
        assert_eq!(
            store.snapshots.lock().unwrap()[0].status,
            QueryStatus::Ready,
            "permanent gaps are terminal when no retry is pending"
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
    fn test_retryable_response_uses_analysis_retry_without_legacy_fields() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("calls", &args)
            .with_analysis_scope("local".into())
            .with_analysis_summary("building analysis".into())
            .with_analysis_retry_after_ms(2000)
            .with_gap_records(vec![GapRecord {
                scope: "pending".into(),
                reason: "temporary".into(),
                detail: "must not escape before terminal state".into(),
            }])
            .with_is_error(false);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &store);
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["analysis"]["retry_after_ms"], 2000);
        assert!(v.get("partial_result").is_none());
        assert!(v.get("background_refinement").is_none());
        assert!(v.get("gaps").is_none());
        assert_eq!(
            store.snapshots.lock().unwrap()[0].status,
            QueryStatus::Retryable
        );
    }

    #[test]
    fn test_lazy_response_explicit_analysis_fields() {
        let store = MockStore::new();
        let args = json!({"symbol": "test"});
        let lr = AnalysisEnvelope::new("explore", &args)
            .with_analysis_scope("local".into())
            .with_analysis_summary("custom summary".into())
            .with_analysis_basis(vec!["cfg".into()])
            .with_analysis_retry_after_ms(2000)
            .with_is_error(false);
        let body = json!({"ok": true});
        let (json_str, _) = lr.build(body, &store);
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["analysis"]["scope"], "local");
        assert_eq!(v["analysis"]["summary"], "custom summary");
        assert_eq!(v["analysis"]["basis"], json!(["cfg"]));
        assert_eq!(v["analysis"]["retry_after_ms"], 2000);
        for retired in ["unit", "coverage", "missing", "state", "next_action"] {
            assert!(
                v["analysis"].get(retired).is_none(),
                "retired analysis field {retired} must be absent"
            );
        }
        assert!(v.get("work").is_none());
    }

    #[test]
    fn test_full_response_envelope() {
        let args = json!({"symbol": "test_fn"});
        let mut counts = HashMap::new();
        counts.insert("repo_complete".to_string(), 5usize);

        let lr = AnalysisEnvelope::new("test_tool", &args)
            .with_coverage_counts(counts)
            .with_gap_records(vec![GapRecord {
                scope: "a.c".into(),
                reason: "unresolved_import".into(),
                detail: "Import 'b.h' could not be resolved from this file.".into(),
            }])
            .with_is_error(false);

        let body = json!({"result": "ok"});
        let (json_str, is_err) = lr.build(body, &MockStore::new());

        assert!(!is_err);
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
        assert!(json_str.contains("\"gaps\""), "should contain gaps key");
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
