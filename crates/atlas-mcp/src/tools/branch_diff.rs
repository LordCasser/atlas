//! branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.
//!
//! DEBT-8: handler owns arg parse + envelope render; orchestration lives in
//! [`super::runtime::analysis_runtime::AnalysisRuntime::run_branch_diff`].

use super::ToolRouter;
use super::analysis_envelope::{AnalysisEnvelope, GapRecord};
use crate::tools::symbol_selector::{
    SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};
use atlas_engine::LazyWindow;
use atlas_engine::structs::{CoverageTier, FactCoverage};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchDiffAnalysisMode {
    CfgOnly,
    SemanticReady,
    SemanticBoundary { has_dataflow: bool },
    SemanticUnavailable,
}

fn lazy_window_has_boundary(window: &LazyWindow) -> bool {
    window.truncated
        || window.units_pending > 0
        || !window.pending_job_ids.is_empty()
        || window.quality.as_ref().is_some_and(|precision| {
            matches!(
                precision.coverage,
                CoverageTier::Boundary { .. }
                    | CoverageTier::Partial { .. }
                    | CoverageTier::Manifest
            )
        })
}

fn classify_branch_diff_analysis(
    use_semantic: bool,
    semantic_window: Option<&LazyWindow>,
    dataflow_refinement_failed: bool,
) -> BranchDiffAnalysisMode {
    if !use_semantic {
        return BranchDiffAnalysisMode::CfgOnly;
    }

    if dataflow_refinement_failed {
        return BranchDiffAnalysisMode::SemanticUnavailable;
    }

    let Some(window) = semantic_window else {
        return BranchDiffAnalysisMode::SemanticBoundary {
            has_dataflow: false,
        };
    };

    let has_dataflow = window.capability_mask.has(FactCoverage::DATAFLOW);
    if has_dataflow && !lazy_window_has_boundary(window) {
        BranchDiffAnalysisMode::SemanticReady
    } else {
        BranchDiffAnalysisMode::SemanticBoundary { has_dataflow }
    }
}

fn apply_branch_diff_analysis(
    lr: AnalysisEnvelope,
    mode: BranchDiffAnalysisMode,
) -> AnalysisEnvelope {
    match mode {
        BranchDiffAnalysisMode::SemanticBoundary { has_dataflow } => {
            let mut basis = vec!["cfg".into()];
            if has_dataflow {
                basis.extend(["dataflow".into(), "effects".into()]);
            }

            lr.with_analysis_scope("local".into())
                .with_analysis_basis(basis)
                .with_analysis_summary(
                    "Branch diff used the focused function context; nearby semantic facts are still being expanded."
                        .into(),
                )
                .with_analysis_retry_after_ms(8000)
        }
        BranchDiffAnalysisMode::SemanticUnavailable => {
            lr.with_analysis_scope("local".into())
                .with_analysis_basis(vec!["cfg".into()])
                .with_gap_records(vec![GapRecord {
                    scope: "current function".into(),
                    reason: "no_dataflow".into(),
                    detail: "Dataflow refinement failed for this function. The analysis fell back to CFG-only effects. Check that the source file compiles successfully and the function uses recognized memory-allocation patterns.".into(),
                }])
                .with_analysis_summary(
                    "Semantic branch diff fell back to CFG-only effects because dataflow facts are unavailable."
                    .into(),
                )
        }
        BranchDiffAnalysisMode::SemanticReady => lr
            .with_analysis_scope("local".into())
            .with_analysis_basis(vec!["cfg".into(), "dataflow".into(), "effects".into()])
            .with_gap_records(vec![])
            .with_analysis_summary(
                "Semantic branch diff used complete focused function CFG and dataflow effects.".into(),
            ),
        BranchDiffAnalysisMode::CfgOnly => lr
            .with_analysis_scope("local".into())
            .with_analysis_basis(vec!["cfg".into()])
            .with_gap_records(vec![])
            .with_analysis_summary("Branch diff used complete focused function CFG effects.".into()),
    }
}

impl ToolRouter {
    pub(crate) fn handle_branch_diff(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let symbol = match &input {
            SymbolInput::Name(s) => s.clone(),
            SymbolInput::Selector(sel) => sel.qualified_name.clone(),
        };
        if let Err(e) = super::validate_symbol_name_length(&symbol) {
            return (e, true);
        }

        if symbol.is_empty() {
            return ("Missing required parameter: symbol".to_string(), true);
        }

        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        let mut lr = AnalysisEnvelope::new("branch_diff", args).with_root_warnings(root_warnings);
        let query_id = lr.query_id().to_string();
        let (focus_result, focus_warnings) = self.prepare_focus_query_with_roots(
            Some(atlas_engine::QueryIntent::SemanticFunction {
                symbol_name: symbol.clone(),
                file_id: self.resolve_selector_file_id(&input),
                symbol_id: None,
            }),
            include_roots,
        );
        if let Some(ref result) = focus_result {
            lr = crate::tools::apply_focus_result_to_lr(lr, result);
        }
        if !focus_warnings.is_empty() {
            lr = lr.with_lazy_warnings(focus_warnings);
        }

        let sid = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle)
        {
            Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                return super::format_ambiguous_error(&candidates, &symbol);
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                return self.retryable_symbol_not_found_response(
                    "branch_diff",
                    args,
                    &symbol,
                    Vec::new(),
                    Some("branch_diff requires the function CFG to be materialized first".into()),
                );
            }
            Err(e) => return (e, true),
        };

        // Check for semantic analysis mode (default: true)
        let use_semantic = args
            .get("semantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Dispatcher owns CFG ensure, store I/O, composition, engine call.
        let analysis = match self.project().analysis_runtime.run_branch_diff(
            &self.project().store,
            &sid,
            &query_id,
            &symbol,
            use_semantic,
        ) {
            Ok(ok) => ok,
            Err(e) => {
                let resp = json!({
                    "ok": false,
                    "function": symbol,
                    "error": e,
                });
                return lr.with_is_error(true).build(resp, self);
            }
        };

        if analysis.dataflow_refinement_failed {
            lr = lr.with_root_warnings(vec![format!(
                "Semantic dataflow refinement failed for '{symbol}'"
            )]);
        }

        let diffs = &analysis.diffs;
        let mut resp = json!({
            "ok": true,
            "function": analysis.qname,
            "branch_count": diffs.len(),
            "branches": diffs.iter().map(|d| json!({
                "line": d.branch_node_line.saturating_add(1),
                "common_field": d.common_prefix,
                "true_path": {
                    "frees": d.path_true.frees,
                    "allocates": d.path_true.allocates,
                    "writes": d.path_true.writes,
                    "reads": d.path_true.reads,
                },
                "false_path": {
                    "frees": d.path_false.frees,
                    "allocates": d.path_false.allocates,
                    "writes": d.path_false.writes,
                    "reads": d.path_false.reads,
                },
                "asymmetry": d.suspicious_asymmetry,
            })).collect::<Vec<_>>(),
        });

        // Add diagnostic when no branches found
        if diffs.is_empty() {
            resp["diagnostic"] = json!({
                "message": "No branch asymmetries found. The function may have straight-line code without branch points, or the CFG analysis did not detect sibling branches.",
                "suggestion": "Verify the function contains if/else or switch constructs. For C/C++, ensure the source file compiles to produce accurate CFG data."
            });
        }

        let analysis_mode = classify_branch_diff_analysis(
            use_semantic,
            analysis.semantic_window.as_ref(),
            analysis.dataflow_refinement_failed,
        );
        lr = apply_branch_diff_analysis(lr, analysis_mode);
        lr.with_is_error(false).build(resp, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::analysis_envelope::SnapshotStore;
    use crate::tools::query_snapshot::QuerySnapshot;
    use atlas_engine::structs::{AnswerQuality, SemanticConfidence, SymbolTier};
    use atlas_engine::{AnalysisUnit, FileId, SymbolId, TextRange};
    use serde_json::json;
    use std::sync::Mutex;

    struct MockStore {
        snapshots: Mutex<Vec<QuerySnapshot>>,
    }

    impl SnapshotStore for MockStore {
        fn store_query_snapshot(&self, snapshot: QuerySnapshot) {
            self.snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(snapshot);
        }
    }

    fn analysis_json_for(mode: BranchDiffAnalysisMode) -> serde_json::Value {
        let lr = AnalysisEnvelope::new("branch_diff", &json!({"symbol": "f"}));
        let lr = apply_branch_diff_analysis(lr, mode).with_is_error(false);
        let (text, err) = lr.build(
            json!({"ok": true}),
            &MockStore {
                snapshots: Mutex::new(vec![]),
            },
        );
        assert!(!err);
        serde_json::from_str(&text).unwrap()
    }

    fn assert_public_analysis_schema(value: &serde_json::Value) {
        for retired in ["unit", "coverage", "missing", "state", "next_action"] {
            assert!(
                value["analysis"].get(retired).is_none(),
                "retired analysis field {retired} must be absent: {value}"
            );
        }
    }

    fn lazy_window(capability_mask: FactCoverage, pending: bool) -> LazyWindow {
        let seed_unit = AnalysisUnit::from_function(
            FileId::default(),
            SymbolId::default(),
            TextRange::default(),
        );
        LazyWindow {
            seed_unit: seed_unit.clone(),
            units: vec![seed_unit],
            variable_focus: None,
            truncated: false,
            units_built: 1,
            units_cached: 0,
            units_pending: usize::from(pending),
            pending_job_ids: vec![],
            quality: Some(if pending {
                AnswerQuality {
                    coverage: CoverageTier::Boundary {
                        target_tier: SymbolTier::Full,
                    },
                    confidence: SemanticConfidence::High,
                }
            } else {
                AnswerQuality::best()
            }),
            capability_mask,
        }
    }

    #[test]
    fn cfg_only_branch_diff_is_function_complete() {
        let value = analysis_json_for(BranchDiffAnalysisMode::CfgOnly);
        assert_eq!(value["analysis"]["scope"], "local");
        assert_eq!(value["analysis"]["basis"], json!(["cfg"]));
        assert_public_analysis_schema(&value);
        assert!(value["analysis"].get("retry_after_ms").is_none());
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn semantic_branch_diff_empty_dataflow_but_capability_ready_is_function_complete() {
        let mut mask = FactCoverage::default();
        mask.set(FactCoverage::DATAFLOW);
        let window = lazy_window(mask, false);
        let mode = classify_branch_diff_analysis(true, Some(&window), false);
        assert_eq!(mode, BranchDiffAnalysisMode::SemanticReady);

        let value = analysis_json_for(mode);
        assert_eq!(value["analysis"]["scope"], "local");
        assert_eq!(
            value["analysis"]["basis"],
            json!(["cfg", "dataflow", "effects"])
        );
        assert_public_analysis_schema(&value);
        assert!(value["analysis"].get("retry_after_ms").is_none());
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn semantic_branch_diff_boundary_with_dataflow_is_waitable() {
        let mut mask = FactCoverage::default();
        mask.set(FactCoverage::DATAFLOW);
        let window = lazy_window(mask, true);
        let mode = classify_branch_diff_analysis(true, Some(&window), false);
        assert_eq!(
            mode,
            BranchDiffAnalysisMode::SemanticBoundary { has_dataflow: true }
        );

        let value = analysis_json_for(mode);
        assert_eq!(value["analysis"]["scope"], "local");
        assert_eq!(
            value["analysis"]["basis"],
            json!(["cfg", "dataflow", "effects"])
        );
        assert_public_analysis_schema(&value);
        // Non-terminal response must NOT include gaps
        assert!(
            value.get("gaps").is_none(),
            "gaps must not appear in non-terminal response"
        );
        assert_eq!(value["analysis"]["retry_after_ms"], 8000);
        assert!(
            value.get("partial_result").is_none(),
            "partial_result must not be set"
        );
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn semantic_branch_diff_failed_dataflow_refinement_does_not_suggest_waiting() {
        let mode = classify_branch_diff_analysis(true, None, true);
        assert_eq!(mode, BranchDiffAnalysisMode::SemanticUnavailable);

        let value = analysis_json_for(mode);
        assert_eq!(value["analysis"]["scope"], "local");
        assert_public_analysis_schema(&value);
        let gaps = value["gaps"].as_array().expect("gaps should be present");
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0]["scope"], "current function");
        assert_eq!(gaps[0]["reason"], "no_dataflow");
        assert!(
            gaps[0]["detail"]
                .as_str()
                .unwrap()
                .contains("Dataflow refinement failed"),
        );
        assert!(value["analysis"].get("retry_after_ms").is_none());
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn branch_diff_zero_branches_has_diagnostic() {
        // Simulate handle_branch_diff's response-building when branch_count is 0.
        // We don't invoke the full handler; we construct the same JSON shape.
        let diffs: Vec<atlas_engine::analysis::BranchDiff> = Vec::new();
        let mut resp = json!({
            "ok": true,
            "function": "test_func",
            "branch_count": diffs.len(),
            "branches": diffs.iter().map(|d| json!({
                "line": d.branch_node_line.saturating_add(1),
                "common_field": d.common_prefix,
                "true_path": {
                    "frees": d.path_true.frees,
                    "allocates": d.path_true.allocates,
                    "writes": d.path_true.writes,
                    "reads": d.path_true.reads,
                },
                "false_path": {
                    "frees": d.path_false.frees,
                    "allocates": d.path_false.allocates,
                    "writes": d.path_false.writes,
                    "reads": d.path_false.reads,
                },
                "asymmetry": d.suspicious_asymmetry,
            })).collect::<Vec<_>>(),
        });

        // Add diagnostic when no branches found (same logic as handle_branch_diff)
        if diffs.is_empty() {
            resp["diagnostic"] = json!({
                "message": "No branch asymmetries found. The function may have straight-line code without branch points, or the CFG analysis did not detect sibling branches.",
                "suggestion": "Verify the function contains if/else or switch constructs. For C/C++, ensure the source file compiles to produce accurate CFG data."
            });
        }

        let diagnostic = resp.get("diagnostic").expect("diagnostic should exist");
        assert!(!diagnostic["message"].as_str().unwrap().is_empty());
        assert!(!diagnostic["suggestion"].as_str().unwrap().is_empty());
    }

    #[test]
    fn semantic_branch_diff_missing_is_descriptive() {
        // SemanticBoundary is non-terminal → gaps must NOT appear
        let value =
            analysis_json_for(BranchDiffAnalysisMode::SemanticBoundary { has_dataflow: true });
        assert!(
            value.get("gaps").is_none(),
            "gaps must not appear in non-terminal SemanticBoundary response"
        );

        let value2 = analysis_json_for(BranchDiffAnalysisMode::SemanticBoundary {
            has_dataflow: false,
        });
        assert!(
            value2.get("gaps").is_none(),
            "gaps must not appear in non-terminal SemanticBoundary response"
        );

        // SemanticUnavailable is terminal → gaps SHOULD appear with reason "no_dataflow"
        let value3 = analysis_json_for(BranchDiffAnalysisMode::SemanticUnavailable);
        let gaps3 = value3["gaps"].as_array().unwrap();
        assert_eq!(gaps3.len(), 1);
        assert_eq!(
            gaps3[0]["reason"].as_str().unwrap(),
            "no_dataflow",
            "gaps should contain no_dataflow, got: {}",
            gaps3[0]
        );
    }
}
