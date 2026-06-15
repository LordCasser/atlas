//! branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.

use super::analysis_envelope::AnalysisEnvelope;
use super::ToolRouter;
use crate::tools::symbol_selector::{
    parse_symbol_input, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
};
use atlas_engine::structs::{CapabilityMask, CoverageTier};
use atlas_engine::LazyWindow;
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
        || window.precision.as_ref().is_some_and(|precision| {
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

    let has_dataflow = window.capability_mask.has(CapabilityMask::DATAFLOW);
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
            let missing = if has_dataflow {
                basis.extend(["dataflow".into(), "effects".into()]);
                vec!["closure_refinement".into()]
            } else {
                vec!["dataflow".into()]
            };

            lr.with_analysis_state("boundary".into())
                .with_analysis_scope("local".into())
                .with_analysis_unit("function".into())
                .with_analysis_coverage("boundary_partial".into())
                .with_analysis_basis(basis)
                .with_analysis_missing(missing)
                .with_analysis_summary(
                    "Branch diff used the focused function context; nearby semantic facts are still being expanded."
                        .into(),
                )
                .with_analysis_next_action("wait_then_resume".into())
                .with_analysis_retry_after_ms(2000)
                .with_partial_result(true)
        }
        BranchDiffAnalysisMode::SemanticUnavailable => {
            lr.with_analysis_state("degraded".into())
                .with_analysis_scope("local".into())
                .with_analysis_unit("function".into())
                .with_analysis_coverage("function_complete".into())
                .with_analysis_basis(vec!["cfg".into()])
                .with_analysis_missing(vec!["dataflow".into()])
                .with_analysis_summary(
                    "Semantic branch diff fell back to CFG-only effects because dataflow facts are unavailable."
                    .into(),
                )
                .with_analysis_next_action("run_full_index".into())
                .with_partial_result(true)
        }
        BranchDiffAnalysisMode::SemanticReady => lr
            .with_analysis_state("ready".into())
            .with_analysis_scope("local".into())
            .with_analysis_unit("function".into())
            .with_analysis_coverage("function_complete".into())
            .with_analysis_basis(vec!["cfg".into(), "dataflow".into(), "effects".into()])
            .with_analysis_missing(vec![])
            .with_analysis_summary(
                "Semantic branch diff used complete focused function CFG and dataflow effects.".into(),
            )
            .with_analysis_next_action("use_result".into()),
        BranchDiffAnalysisMode::CfgOnly => lr
            .with_analysis_state("ready".into())
            .with_analysis_scope("local".into())
            .with_analysis_unit("function".into())
            .with_analysis_coverage("function_complete".into())
            .with_analysis_basis(vec!["cfg".into()])
            .with_analysis_missing(vec![])
            .with_analysis_summary("Branch diff used complete focused function CFG effects.".into())
            .with_analysis_next_action("use_result".into()),
    }
}

impl ToolRouter {
    pub(crate) fn handle_branch_diff(&mut self, args: &serde_json::Value) -> (String, bool) {
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

        let mut lr = AnalysisEnvelope::new("branch_diff", args);
        let query_id = lr.query_id().to_string();

        // Resolve symbol to SymbolId. When a structured selector includes a
        // file_path, focus can seed that file before retrying resolution.
        let sid = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::BestEffortSingle,
            None,
            None,
        ) {
            Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                return super::format_ambiguous_error(&candidates, &symbol);
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                let mut err = format!("Symbol not found: {symbol}");
                err.push_str(self.active_mut().store_query_runtime.not_indexed_guidance());
                return (err, true);
            }
            Err(e) => return (e, true),
        };

        // Ensure structural data is available
        if let Ok(Some(sym)) = self.active_mut().store.find_symbol_by_id(&sid) {
            let (focus_result, focus_warnings) =
                self.prepare_focus_query(Some(atlas_engine::QueryIntent::Calls {
                    symbol_name: sym.name.clone(),
                    file_id: Some(sym.file_id),
                    symbol_id: None,
                    direction: None,
                    depth: None,
                }));
            if let Some(ref result) = focus_result {
                lr = crate::tools::apply_focus_result_to_lr(lr, result);
            }
            if !focus_warnings.is_empty() {
                lr = lr.with_lazy_warnings(focus_warnings);
            }
        }

        // Load CFG nodes for this function, with lazy CFG fallback
        let store = self.active().store.clone();
        let (cfg_nodes, cfg_edges) = match self
            .active_mut()
            .analysis_runtime
            .ensure_cfg_for_function(&store, &sid, &query_id, &symbol)
        {
            Ok((nodes, edges)) => (nodes, edges),
            Err(e) => {
                let resp = json!({
                    "ok": false,
                    "function": symbol,
                    "error": e,
                });
                return lr.with_is_error(true).build(resp, self);
            }
        };

        // --- CFG is available — run branch diff analysis ---

        let qname = self
            .active_mut()
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.qualified_name)
            .unwrap_or_else(|| symbol.to_string());

        // Check for semantic analysis mode (default: true)
        let use_semantic = args
            .get("semantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mut dataflow_refinement_failed = false;
        let mut semantic_window = None;
        let diffs = if use_semantic {
            // ── SEMANTIC PATH: compose_effects + diff_branches_semantic ──
            let lang = self
                .active_mut()
                .store
                .find_symbol_by_id(&sid)
                .ok()
                .flatten()
                .map(|s| s.language)
                .unwrap_or(atlas_engine::Language::C);
            let contract = atlas_engine::analysis::ResourceOpConfig::default_for(lang);

            match self
                .active_mut()
                .analysis_runtime
                .ensure_dataflow_for_function(&sid, Some(&query_id))
            {
                Ok(window) => semantic_window = Some(window),
                Err(e) => {
                    dataflow_refinement_failed = true;
                    lr = lr.with_root_warnings(vec![format!(
                        "Semantic dataflow refinement failed for '{symbol}': {e:#}"
                    )]);
                }
            }

            // Load DataFlow nodes and edges
            let data_nodes = self
                .active_mut()
                .store
                .find_data_nodes_by_function(&sid)
                .unwrap_or_default();
            let dataflow_edges = if data_nodes.is_empty() {
                vec![]
            } else {
                let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
                self.active_mut()
                    .store
                    .find_dataflow_edges_by_sources(&all_ids)
                    .unwrap_or_default()
            };

            let composition =
                match atlas_engine::analysis::cfg_graph::CfgGraph::build(&cfg_nodes, &cfg_edges) {
                    Ok(cfg_graph) => atlas_engine::analysis::compose_effects(
                        &cfg_graph,
                        &data_nodes,
                        &dataflow_edges,
                        &contract,
                    ),
                    Err(_) => {
                        // CFG build failed → fall back to minimal composition
                        atlas_engine::analysis::EffectComposition::default()
                    }
                };

            atlas_engine::analysis::BranchDiffEngine::diff_branches_semantic(
                &cfg_nodes,
                &cfg_edges,
                &composition,
            )
        } else {
            // ── BASIC PATH: CFG-only diff (effect_kind based) ──
            atlas_engine::analysis::BranchDiffEngine::diff_branches(&cfg_nodes, &cfg_edges)
        };

        let resp = json!({
            "ok": true,
            "function": qname,
            "branch_count": diffs.len(),
            "branches": diffs.iter().map(|d| json!({
                "line": d.branch_node_line,
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

        let analysis_mode = classify_branch_diff_analysis(
            use_semantic,
            semantic_window.as_ref(),
            dataflow_refinement_failed,
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
    use atlas_engine::structs::{Precision, SemanticConfidence, SymbolTier};
    use atlas_engine::{AnalysisUnit, FileId, SymbolId, TextRange};
    use serde_json::json;

    struct MockStore {
        snapshots: Vec<QuerySnapshot>,
    }

    impl SnapshotStore for MockStore {
        fn store_query_snapshot(&mut self, snapshot: QuerySnapshot) {
            self.snapshots.push(snapshot);
        }
    }

    fn analysis_json_for(mode: BranchDiffAnalysisMode) -> serde_json::Value {
        let lr = AnalysisEnvelope::new("branch_diff", &json!({"symbol": "f"}));
        let lr = apply_branch_diff_analysis(lr, mode).with_is_error(false);
        let (text, err) = lr.build(json!({"ok": true}), &mut MockStore { snapshots: vec![] });
        assert!(!err);
        serde_json::from_str(&text).unwrap()
    }

    fn lazy_window(capability_mask: CapabilityMask, pending: bool) -> LazyWindow {
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
            precision: Some(if pending {
                Precision {
                    coverage: CoverageTier::Boundary {
                        target_tier: SymbolTier::Full,
                    },
                    confidence: SemanticConfidence::High,
                }
            } else {
                Precision::best()
            }),
            capability_mask,
        }
    }

    #[test]
    fn cfg_only_branch_diff_is_function_complete() {
        let value = analysis_json_for(BranchDiffAnalysisMode::CfgOnly);
        assert_eq!(value["analysis"]["state"], "ready");
        assert_eq!(value["analysis"]["unit"], "function");
        assert_eq!(value["analysis"]["coverage"], "function_complete");
        assert_eq!(value["analysis"]["basis"], json!(["cfg"]));
        assert_eq!(value["analysis"]["missing"], json!([]));
        assert_eq!(value["analysis"]["next_action"], "use_result");
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn semantic_branch_diff_empty_dataflow_but_capability_ready_is_function_complete() {
        let mut mask = CapabilityMask::default();
        mask.set(CapabilityMask::DATAFLOW);
        let window = lazy_window(mask, false);
        let mode = classify_branch_diff_analysis(true, Some(&window), false);
        assert_eq!(mode, BranchDiffAnalysisMode::SemanticReady);

        let value = analysis_json_for(mode);
        assert_eq!(value["analysis"]["state"], "ready");
        assert_eq!(value["analysis"]["unit"], "function");
        assert_eq!(value["analysis"]["coverage"], "function_complete");
        assert_eq!(
            value["analysis"]["basis"],
            json!(["cfg", "dataflow", "effects"])
        );
        assert_eq!(value["analysis"]["missing"], json!([]));
        assert_eq!(value["analysis"]["next_action"], "use_result");
        assert!(value["analysis"].get("retry_after_ms").is_none());
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn semantic_branch_diff_boundary_with_dataflow_is_waitable() {
        let mut mask = CapabilityMask::default();
        mask.set(CapabilityMask::DATAFLOW);
        let window = lazy_window(mask, true);
        let mode = classify_branch_diff_analysis(true, Some(&window), false);
        assert_eq!(
            mode,
            BranchDiffAnalysisMode::SemanticBoundary { has_dataflow: true }
        );

        let value = analysis_json_for(mode);
        assert_eq!(value["analysis"]["state"], "boundary");
        assert_eq!(value["analysis"]["unit"], "function");
        assert_eq!(value["analysis"]["coverage"], "boundary_partial");
        assert_eq!(
            value["analysis"]["basis"],
            json!(["cfg", "dataflow", "effects"])
        );
        assert_eq!(value["analysis"]["missing"], json!(["closure_refinement"]));
        assert_eq!(value["analysis"]["next_action"], "wait_then_resume");
        assert_eq!(value["analysis"]["retry_after_ms"], 2000);
        assert!(value["partial_result"].as_bool().unwrap());
        assert!(value.get("work").is_none(), "work must not be public");
    }

    #[test]
    fn semantic_branch_diff_failed_dataflow_refinement_does_not_suggest_waiting() {
        let mode = classify_branch_diff_analysis(true, None, true);
        assert_eq!(mode, BranchDiffAnalysisMode::SemanticUnavailable);

        let value = analysis_json_for(mode);
        assert_eq!(value["analysis"]["state"], "degraded");
        assert_eq!(value["analysis"]["unit"], "function");
        assert_eq!(value["analysis"]["coverage"], "function_complete");
        assert_eq!(value["analysis"]["missing"], json!(["dataflow"]));
        assert_eq!(value["analysis"]["next_action"], "run_full_index");
        assert!(value["analysis"].get("retry_after_ms").is_none());
        assert!(value.get("work").is_none(), "work must not be public");
    }
}
