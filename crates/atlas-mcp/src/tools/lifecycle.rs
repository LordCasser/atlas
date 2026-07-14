//! lifecycle — field lifecycle analysis using CFG effect annotations.
//!
//! Given a function symbol and a field path, walks the function's CFG nodes
//! with effect annotations to produce a state-machine view of the field's
//! lifecycle: allocation, use, escape, free, and suspicious patterns (use-after-free, double-free).
//!
//! DEBT-8: handler owns arg parse + envelope render; orchestration lives in
//! [`super::runtime::analysis_runtime::AnalysisRuntime::run_lifecycle`].

use super::analysis_envelope::{AnalysisEnvelope, GapRecord};
use super::runtime::analysis_runtime::LifecycleAnalysisErr;
use super::{ToolRouter, get_str};
use crate::tools::symbol_selector::{
    SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_lifecycle(&self, args: &serde_json::Value) -> (String, bool) {
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
        let field_raw = get_str(args, "field");
        let field = atlas_engine::canonicalize_field_path(field_raw);

        if symbol.is_empty() || field.is_empty() {
            return (
                "Missing required parameters: symbol and field".to_string(),
                true,
            );
        }

        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        let mut lr = AnalysisEnvelope::new("lifecycle", args).with_root_warnings(root_warnings);
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
        if focus_result
            .as_ref()
            .is_some_and(|result| result.pending_work_count_and_eta_ms().0 > 0)
        {
            return lr
                .with_is_error(false)
                .build(json!({"status": "in_progress"}), self);
        }

        // Resolve symbol to SymbolId
        let sid = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle)
        {
            Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                return super::format_ambiguous_error(&candidates, &symbol);
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                return self.retryable_symbol_not_found_response(
                    "lifecycle",
                    args,
                    &symbol,
                    Vec::new(),
                    Some(
                        "lifecycle analysis requires the function CFG to be materialized first"
                            .into(),
                    ),
                );
            }
            Err(e) => return (e, true),
        };

        // Dispatcher owns capability gate, store I/O, composition, engine call.
        let analysis = match self.project().analysis_runtime.run_lifecycle(
            &self.project().store,
            &sid,
            &field,
            &query_id,
            &symbol,
        ) {
            Ok(ok) => ok,
            Err(LifecycleAnalysisErr::CfgUnavailable(e)) => {
                let resp = json!({
                    "ok": false,
                    "function": symbol,
                    "field_path": field,
                    "error": e,
                });
                return lr.with_is_error(true).build(resp, self);
            }
            Err(LifecycleAnalysisErr::UnsupportedLanguage) => {
                let resp = json!({
                    "ok": false,
                    "function": symbol,
                    "field_path": field,
                    "error": "unsupported_language",
                    "message": "Lifecycle analysis only supports C/C++. The requested symbol is not C/C++ or could not be resolved.",
                });
                return lr
                    .with_is_error(false)
                    .with_analysis_scope("local".into())
                    .with_analysis_summary(
                        "Lifecycle analysis is unavailable for this language; only C/C++ are supported."
                            .into(),
                    )
                    .with_analysis_basis(vec!["cfg".into()])
                    .with_gap_records(vec![GapRecord {
                        scope: symbol.clone(),
                        reason: "unsupported_language".into(),
                        detail: "Lifecycle analysis currently supports only C/C++ symbols.".into(),
                    }])
                    .build(resp, self);
            }
        };

        let result = analysis.result;
        let proof = atlas_engine::analysis::evaluate_proof(
            &result.suspicious_points,
            result.final_state,
            analysis.has_user_rules,
            analysis.has_any_rules,
        );

        let proof_path = atlas_engine::analysis::PathProof {
            conditions: Vec::new(),
            states: result
                .transitions
                .iter()
                .map(|t| (t.node_line, t.to_state.as_str().to_string()))
                .collect(),
            exit_state: result.final_state,
        };

        let resp = json!({
            "ok": true,
            "field_path": result.field_path,
            "function": result.function_qname,
            "final_state": result.final_state.as_str(),
            "verdict": proof.verdict.as_str(),
            "evidence_level": proof.evidence_level.as_str(),
            "reasoning": proof.reasoning,
            "proof_paths": [{
                "states": proof_path.states.iter().map(|(line, state)| json!({
                    "line": line,
                    "state": state,
                })).collect::<Vec<_>>(),
                "exit_state": proof_path.exit_state.as_str(),
            }],
            "transitions_count": result.transitions.len(),
            "transitions": result.transitions.iter().map(|t| json!({
                "from": t.from_state.as_str(),
                "to": t.to_state.as_str(),
                "line": t.node_line,
                "reason": t.effect.map(|e| format!("{e:?}")).unwrap_or_else(|| "transition".to_string()),
            })).collect::<Vec<_>>(),
            "suspicious_count": result.suspicious_points.len(),
            "suspicious": result.suspicious_points.iter().map(|p| json!({
                "line": p.line,
                "kind": format!("{:?}", p.kind),
                "message": p.message,
            })).collect::<Vec<_>>(),
        });

        if let Some(error) = analysis.dataflow_error {
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_basis(vec!["cfg".into(), "domain_rules".into()])
                .with_gap_records(vec![super::analysis_envelope::GapRecord {
                    scope: result.function_qname.clone(),
                    reason: "no_dataflow".into(),
                    detail: error,
                }])
                .with_analysis_summary(format!(
                    "Lifecycle dataflow refinement failed; CFG-only analysis found {} transitions, final state={}, {} suspicious point(s).",
                    result.transitions.len(),
                    result.final_state.as_str(),
                    result.suspicious_points.len(),
                ));
        } else if result.partial {
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_basis(vec![
                    "cfg".into(),
                    "dataflow".into(),
                    "effects".into(),
                    "domain_rules".into(),
                ])
                .with_gap_records(vec![super::analysis_envelope::GapRecord {
                    scope: result.function_qname.clone(),
                    reason: "analysis_budget".into(),
                    detail: "Lifecycle fixpoint reached its bounded visit limit.".into(),
                }])
                .with_analysis_summary(format!(
                    "Lifecycle analysis hit its bounded visit limit: {} transitions found, final state={}, {} suspicious point(s).",
                    result.transitions.len(),
                    result.final_state.as_str(),
                    result.suspicious_points.len(),
                ));
        } else {
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_basis(vec![
                    "cfg".into(),
                    "dataflow".into(),
                    "effects".into(),
                    "domain_rules".into(),
                ])
                .with_gap_records(vec![])
                .with_analysis_summary(
                    "Lifecycle analysis used focused CFG, dataflow effects, and domain rules."
                        .into(),
                );
        }
        lr.with_is_error(false).build(resp, self)
    }
}
