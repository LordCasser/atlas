//! Unified analysis response envelope types for MCP tool responses.
//!
//! Replaces the hand-built response envelope (precision_tier, hint, lazy_diagnostics,
//! analysis_contract, pending_closures, etc.) with a structured `AnalysisResponse`
//! that wraps any tool-specific result body under a stable user/agent-facing envelope.
//!
//! # Architecture invariant
//! - Internal focus/lazy concepts (closure_id, PrecisionTier enum values,
//!   lazy_diagnostics internals, pending_closures, focus scheduler priorities)
//!   MUST NOT appear in MCP responses.
//! - Public coverage labels: `repo_complete`, `local_complete`, `boundary`,
//!   `partial`, `basic` — NOT `ClosureComplete` or other internal names.

use std::collections::HashMap;

use serde::Serialize;

// ---------------------------------------------------------------------------
// Top-level envelope
// ---------------------------------------------------------------------------

/// The top-level analysis response envelope.
///
/// Every MCP analysis tool response should use this structure.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisResponse {
    /// Tool-specific result body (the existing response JSON).
    pub result: serde_json::Value,
    /// Stable user/agent-facing analysis state summary.
    pub analysis: AnalysisView,
    /// Precision contract: coverage scope × semantic confidence.
    pub precision: Option<PrecisionView>,
    /// Distribution of results by coverage tier (public labels only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_counts: Option<HashMap<String, usize>>,
    /// Known gaps in analysis completeness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<Vec<KnownGapView>>,
    /// Aggregated background work status.
    pub work: WorkView,
    /// Query identifier for polling / resume.
    pub query_id: String,
}

// ---------------------------------------------------------------------------
// Analysis view (public-facing)
// ---------------------------------------------------------------------------

/// Stable public view of the analysis state.
///
/// Normalized from internal extraction_state, extraction_jobs, focus closures,
/// bootstrap tiers, graph refresh, and query snapshot states.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisView {
    /// Analysis readiness state.
    pub state: AnalysisState,
    /// Scope of the analysis that was performed.
    pub scope: AnalysisScope,
    /// Short human/agent-readable explanation of what is known.
    pub summary: String,
    /// Recommended next action for the user/agent.
    pub next_action: AnalysisNextAction,
}

// ---------------------------------------------------------------------------
// Analysis state enums
// ---------------------------------------------------------------------------

/// Analysis readiness state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisState {
    /// Full results are available; no further work needed for this query.
    Ready,
    /// Partial results available; some evidence is still being refined.
    UsablePartial,
    /// Analysis is actively building — results may change.
    Building,
    /// Blocked by missing data (e.g., no index at all).
    Blocked,
    /// Fatal analysis error.
    Failed,
    /// Previously valid results may be stale.
    Stale,
}

/// Scope of the analysis performed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisScope {
    Repo,
    Local,
    File,
    Symbol,
    Query,
    Corpus,
}

/// Recommended next action.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisNextAction {
    UseResult,
    UseResultOrWaitForRefinement,
    Wait,
    NarrowScope,
    RunFullIndex,
    Retry,
    InspectGaps,
}

// ---------------------------------------------------------------------------
// Precision view
// ---------------------------------------------------------------------------

/// Precision contract visible to users/agents.
#[derive(Debug, Clone, Serialize)]
pub struct PrecisionView {
    /// Public coverage label: repo_complete | local_complete | boundary | partial | basic
    pub coverage: String,
    /// Confidence level: certain | high | medium | low
    pub confidence: String,
}

// ---------------------------------------------------------------------------
// Known gap view
// ---------------------------------------------------------------------------

/// A known gap, using public field names.
#[derive(Debug, Clone, Serialize)]
pub struct KnownGapView {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsite: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_pct: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_hidden_symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_hidden_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Work view
// ---------------------------------------------------------------------------

/// Aggregated background work view.
#[derive(Debug, Clone, Serialize)]
pub struct WorkView {
    /// Whether this work is relevant to this response (vs. global background).
    pub relevant: bool,
    /// Overall work status.
    pub status: WorkStatus,
    /// Individual work items.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<WorkItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Idle,
    Running,
    Completed,
}

/// A single background work item.
#[derive(Debug, Clone, Serialize)]
pub struct WorkItem {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub scope: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<WorkProgress>,
    pub waitable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkProgress {
    pub percent: u8,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`AnalysisResponse`].
pub struct AnalysisResponseBuilder {
    result: serde_json::Value,
    query_id: String,
    analysis: AnalysisView,
    precision: Option<PrecisionView>,
    coverage_counts: Option<HashMap<String, usize>>,
    gaps: Option<Vec<KnownGapView>>,
    work: WorkView,
}

impl AnalysisResponseBuilder {
    /// Create a new builder with the tool-specific result body and query_id.
    ///
    /// Defaults:
    /// - `analysis`: state=Building, scope=Query, summary="", next_action=UseResult
    /// - `work`: status=Idle, items=[]
    pub fn new(result: serde_json::Value, query_id: String) -> Self {
        Self {
            result,
            query_id,
            analysis: AnalysisView {
                state: AnalysisState::Building,
                scope: AnalysisScope::Query,
                summary: String::new(),
                next_action: AnalysisNextAction::UseResult,
            },
            precision: None,
            coverage_counts: None,
            gaps: None,
            work: WorkView {
                relevant: true,
                status: WorkStatus::Idle,
                items: Vec::new(),
            },
        }
    }

    /// Set the analysis view.
    pub fn with_analysis(mut self, analysis: AnalysisView) -> Self {
        self.analysis = analysis;
        self
    }

    /// Set the precision view.
    pub fn with_precision(mut self, precision: PrecisionView) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Set coverage counts (public coverage tier → count).
    pub fn with_coverage_counts(mut self, counts: HashMap<String, usize>) -> Self {
        self.coverage_counts = Some(counts);
        self
    }

    /// Set known gaps.
    pub fn with_gaps(mut self, gaps: Vec<KnownGapView>) -> Self {
        self.gaps = Some(gaps);
        self
    }

    /// Set the work view.
    pub fn with_work(mut self, work: WorkView) -> Self {
        self.work = work;
        self
    }

    /// Consume the builder and produce the [`AnalysisResponse`].
    pub fn build(self) -> AnalysisResponse {
        AnalysisResponse {
            result: self.result,
            analysis: self.analysis,
            precision: self.precision,
            coverage_counts: self.coverage_counts,
            gaps: self.gaps,
            work: self.work,
            query_id: self.query_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

use atlas_engine::structs::{CoverageTier, KnownGap, Precision, SemanticConfidence};

/// Convert a [`Precision`] struct to [`PrecisionView`],
/// mapping internal [`CoverageTier`] to public label.
pub fn precision_to_view(p: &Precision) -> PrecisionView {
    PrecisionView {
        coverage: coverage_tier_to_label(&p.coverage),
        confidence: confidence_to_string(p.confidence).into(),
    }
}

/// Convert a [`KnownGap`] to [`KnownGapView`].
pub fn known_gap_to_view(g: &KnownGap) -> KnownGapView {
    match g {
        KnownGap::UnresolvedImport { from, import_path } => KnownGapView {
            kind: "unresolved_import".into(),
            from: Some(from.clone()),
            import_path: Some(import_path.clone()),
            ..Default::default()
        },
        KnownGap::IndirectCall { callsite, reason } => KnownGapView {
            kind: "indirect_call".into(),
            callsite: Some(callsite.clone()),
            reason: Some(reason.clone()),
            ..Default::default()
        },
        KnownGap::TypeOutside { type_name, ref_by } => KnownGapView {
            kind: "type_outside".into(),
            type_name: Some(type_name.clone()),
            ref_by: Some(ref_by.clone()),
            ..Default::default()
        },
        KnownGap::BudgetExhausted { strategy, remaining } => KnownGapView {
            kind: "budget_exhausted".into(),
            strategy: Some(strategy.clone()),
            remaining: Some(*remaining),
            ..Default::default()
        },
        KnownGap::ConditionalBranch {
            symbol,
            guard,
            branches,
        } => KnownGapView {
            kind: "conditional_branch".into(),
            symbol: Some(symbol.clone()),
            guard: Some(guard.clone()),
            branches: Some(*branches),
            ..Default::default()
        },
        KnownGap::CodeGenerationNotExpanded { at, generator } => KnownGapView {
            kind: "code_generation_not_expanded".into(),
            at: Some(at.clone()),
            generator: Some(generator.clone()),
            ..Default::default()
        },
        KnownGap::HighFanoutName {
            name,
            candidates,
            action,
        } => KnownGapView {
            kind: "high_fanout_name".into(),
            name: Some(name.clone()),
            candidates: Some(*candidates),
            action: Some(action.clone()),
            ..Default::default()
        },
        KnownGap::SymbolHintsIncomplete {
            name,
            coverage_pct,
        } => KnownGapView {
            kind: "symbol_hints_incomplete".into(),
            name: Some(name.clone()),
            coverage_pct: Some(*coverage_pct),
            ..Default::default()
        },
        KnownGap::VisibilityHidden { symbol, reason } => KnownGapView {
            kind: "visibility_hidden".into(),
            visibility_hidden_symbol: Some(symbol.clone()),
            visibility_hidden_reason: Some(reason.clone()),
            ..Default::default()
        },
    }
}

// We need a Default impl for KnownGapView so we can use ..Default::default()
// in the match arms above. Only used internally; not part of the public API.
impl Default for KnownGapView {
    fn default() -> Self {
        Self {
            kind: String::new(),
            from: None,
            import_path: None,
            callsite: None,
            reason: None,
            type_name: None,
            ref_by: None,
            strategy: None,
            remaining: None,
            symbol: None,
            guard: None,
            branches: None,
            at: None,
            generator: None,
            name: None,
            candidates: None,
            action: None,
            coverage_pct: None,
            visibility_hidden_symbol: None,
            visibility_hidden_reason: None,
        }
    }
}

/// Map internal [`CoverageTier`] to public coverage label.
pub fn coverage_tier_to_label(tier: &CoverageTier) -> String {
    match tier {
        CoverageTier::RepoComplete => "repo_complete".into(),
        CoverageTier::ClosureComplete { .. } => "local_complete".into(),
        CoverageTier::Boundary { .. } => "boundary".into(),
        CoverageTier::Partial { .. } => "partial".into(),
        CoverageTier::Manifest => "basic".into(),
    }
}

/// Map internal [`SemanticConfidence`] to public string.
pub fn confidence_to_string(c: SemanticConfidence) -> &'static str {
    match c {
        SemanticConfidence::Certain => "certain",
        SemanticConfidence::High => "high",
        SemanticConfidence::Medium => "medium",
        SemanticConfidence::Low => "low",
    }
}

/// Map internal coverage source strings to public labels.
pub fn coverage_source_to_label(source: &str) -> String {
    match source {
        "extracted_structural" => "local_complete".into(),
        "extracted_resolution_symbols" => "boundary".into(),
        "extracted_manifest" => "basic".into(),
        other => other.into(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::structs::SymbolTier;
    use serde_json::json;

    // ── test 1: minimal empty result ─────────────────────────────────

    #[test]
    fn test_analysis_response_empty_result() {
        let resp = AnalysisResponseBuilder::new(json!({"key": "val"}), "q_001".into())
            .build();
        let json_str = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(v["result"]["key"], "val");
        assert_eq!(v["query_id"], "q_001");
        // analysis should always be present
        assert!(v["analysis"].is_object());
        // precision may be null or absent
        assert!(v["precision"].is_null());
        // work should be present
        assert!(v["work"].is_object());
        assert_eq!(v["work"]["relevant"], true);
        assert_eq!(v["work"]["status"], "idle");
        // coverage_counts and gaps omitted when None
        assert!(v.get("coverage_counts").is_none());
        assert!(v.get("gaps").is_none());
    }

    // ── test 2: AnalysisState serialization ──────────────────────────

    #[test]
    fn test_analysis_state_serialization() {
        let cases = [
            (AnalysisState::Ready, "ready"),
            (AnalysisState::UsablePartial, "usable_partial"),
            (AnalysisState::Building, "building"),
            (AnalysisState::Blocked, "blocked"),
            (AnalysisState::Failed, "failed"),
            (AnalysisState::Stale, "stale"),
        ];
        for (state, expected) in cases {
            let json = serde_json::to_value(&state).unwrap();
            assert_eq!(
                json.as_str().unwrap(),
                expected,
                "AnalysisState::{state:?} serialized incorrectly"
            );
        }
    }

    // ── test 3: AnalysisScope serialization ──────────────────────────

    #[test]
    fn test_analysis_scope_serialization() {
        let cases = [
            (AnalysisScope::Repo, "repo"),
            (AnalysisScope::Local, "local"),
            (AnalysisScope::File, "file"),
            (AnalysisScope::Symbol, "symbol"),
            (AnalysisScope::Query, "query"),
            (AnalysisScope::Corpus, "corpus"),
        ];
        for (scope, expected) in cases {
            let json = serde_json::to_value(&scope).unwrap();
            assert_eq!(json.as_str().unwrap(), expected);
        }
    }

    // ── test 4: AnalysisNextAction serialization ─────────────────────

    #[test]
    fn test_analysis_next_action_serialization() {
        let cases = [
            (AnalysisNextAction::UseResult, "use_result"),
            (
                AnalysisNextAction::UseResultOrWaitForRefinement,
                "use_result_or_wait_for_refinement",
            ),
            (AnalysisNextAction::Wait, "wait"),
            (AnalysisNextAction::NarrowScope, "narrow_scope"),
            (AnalysisNextAction::RunFullIndex, "run_full_index"),
            (AnalysisNextAction::Retry, "retry"),
            (AnalysisNextAction::InspectGaps, "inspect_gaps"),
        ];
        for (action, expected) in cases {
            let json = serde_json::to_value(&action).unwrap();
            assert_eq!(json.as_str().unwrap(), expected);
        }
    }

    // ── test 5: CoverageTier → public label ─────────────────────────

    #[test]
    fn test_precision_view_from_coverage_tier() {
        // ClosureComplete → local_complete
        let ct = CoverageTier::ClosureComplete {
            closure_id: "cl_42".into(),
        };
        assert_eq!(coverage_tier_to_label(&ct), "local_complete");

        // RepoComplete → repo_complete
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::RepoComplete),
            "repo_complete"
        );

        // Boundary → boundary
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::Boundary {
                target_tier: SymbolTier::Full
            }),
            "boundary"
        );

        // Partial → partial
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::Partial { gaps: vec![] }),
            "partial"
        );

        // Manifest → basic
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::Manifest),
            "basic"
        );
    }

    // ── test 6: KnownGap::UnresolvedImport → KnownGapView → JSON ────

    #[test]
    fn test_known_gap_conversion_unresolved_import() {
        let gap = KnownGap::UnresolvedImport {
            from: "src/main.rs".into(),
            import_path: "./lib/helper".into(),
        };
        let view = known_gap_to_view(&gap);
        assert_eq!(view.kind, "unresolved_import");
        assert_eq!(view.from.as_deref(), Some("src/main.rs"));
        assert_eq!(view.import_path.as_deref(), Some("./lib/helper"));

        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["kind"], "unresolved_import");
        assert_eq!(json["from"], "src/main.rs");
        assert_eq!(json["import_path"], "./lib/helper");
        // all other fields absent
        assert!(json.get("callsite").is_none());
        assert!(json.get("symbol").is_none());
    }

    // ── test 7: KnownGap::BudgetExhausted → KnownGapView → JSON ─────

    #[test]
    fn test_known_gap_conversion_budget_exhausted() {
        let gap = KnownGap::BudgetExhausted {
            strategy: "file_limit".into(),
            remaining: 3,
        };
        let view = known_gap_to_view(&gap);
        assert_eq!(view.kind, "budget_exhausted");
        assert_eq!(view.strategy.as_deref(), Some("file_limit"));
        assert_eq!(view.remaining, Some(3));

        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["kind"], "budget_exhausted");
        assert_eq!(json["strategy"], "file_limit");
        assert_eq!(json["remaining"], 3);
    }

    // ── test 8: coverage_source_to_label ─────────────────────────────

    #[test]
    fn test_coverage_source_to_label() {
        assert_eq!(
            coverage_source_to_label("extracted_structural"),
            "local_complete"
        );
        assert_eq!(
            coverage_source_to_label("extracted_resolution_symbols"),
            "boundary"
        );
        assert_eq!(coverage_source_to_label("extracted_manifest"), "basic");
        // unknown sources pass through
        assert_eq!(coverage_source_to_label("custom_source"), "custom_source");
    }

    // ── test 9: confidence_to_string ─────────────────────────────────

    #[test]
    fn test_confidence_to_string() {
        assert_eq!(confidence_to_string(SemanticConfidence::Certain), "certain");
        assert_eq!(confidence_to_string(SemanticConfidence::High), "high");
        assert_eq!(confidence_to_string(SemanticConfidence::Medium), "medium");
        assert_eq!(confidence_to_string(SemanticConfidence::Low), "low");
    }

    // ── test 10: full envelope builder ───────────────────────────────

    #[test]
    fn test_builder_full_envelope() {
        let mut counts = HashMap::new();
        counts.insert("local_complete".into(), 42);
        counts.insert("boundary".into(), 7);

        let gap = KnownGapView {
            kind: "unresolved_import".into(),
            from: Some("a.rs".into()),
            import_path: Some("b.rs".into()),
            ..Default::default()
        };

        let work = WorkView {
            relevant: true,
            status: WorkStatus::Running,
            items: vec![WorkItem {
                id: "job-1".into(),
                kind: "extraction".into(),
                state: "building".into(),
                scope: "file".into(),
                reason: "on_demand".into(),
                progress: Some(WorkProgress { percent: 42 }),
                waitable: true,
                retry_after_ms: Some(500),
            }],
        };

        let analysis = AnalysisView {
            state: AnalysisState::UsablePartial,
            scope: AnalysisScope::Local,
            summary: "3 files extracted, 2 import gaps".into(),
            next_action: AnalysisNextAction::UseResultOrWaitForRefinement,
        };

        let precision = PrecisionView {
            coverage: "local_complete".into(),
            confidence: "high".into(),
        };

        let resp = AnalysisResponseBuilder::new(json!({"symbols": ["a", "b"]}), "q_abc".into())
            .with_analysis(analysis)
            .with_precision(precision)
            .with_coverage_counts(counts)
            .with_gaps(vec![gap])
            .with_work(work)
            .build();

        let json = serde_json::to_value(&resp).unwrap();

        // result
        assert_eq!(json["result"]["symbols"][0], "a");
        assert_eq!(json["result"]["symbols"][1], "b");

        // query_id
        assert_eq!(json["query_id"], "q_abc");

        // analysis
        assert_eq!(json["analysis"]["state"], "usable_partial");
        assert_eq!(json["analysis"]["scope"], "local");
        assert_eq!(json["analysis"]["summary"], "3 files extracted, 2 import gaps");
        assert_eq!(
            json["analysis"]["next_action"],
            "use_result_or_wait_for_refinement"
        );

        // precision
        assert_eq!(json["precision"]["coverage"], "local_complete");
        assert_eq!(json["precision"]["confidence"], "high");

        // coverage_counts
        assert_eq!(json["coverage_counts"]["local_complete"], 42);
        assert_eq!(json["coverage_counts"]["boundary"], 7);

        // gaps
        assert_eq!(json["gaps"][0]["kind"], "unresolved_import");
        assert_eq!(json["gaps"][0]["from"], "a.rs");

        // work
        assert_eq!(json["work"]["relevant"], true);
        assert_eq!(json["work"]["status"], "running");
        assert_eq!(json["work"]["items"][0]["id"], "job-1");
        assert_eq!(json["work"]["items"][0]["kind"], "extraction");
        assert_eq!(json["work"]["items"][0]["progress"]["percent"], 42);
        assert_eq!(json["work"]["items"][0]["waitable"], true);
        assert_eq!(json["work"]["items"][0]["retry_after_ms"], 500);
    }
}
