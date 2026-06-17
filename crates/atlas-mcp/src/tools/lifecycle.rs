//! lifecycle — field lifecycle analysis using CFG effect annotations.
//!
//! Given a function symbol and a field path, walks the function's CFG nodes
//! with effect annotations to produce a state-machine view of the field's
//! lifecycle: allocation, use, escape, free, and suspicious patterns (use-after-free, double-free).

use super::analysis_envelope::{AnalysisEnvelope, GapRecord};
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

        let mut lr = AnalysisEnvelope::new("lifecycle", args);
        let query_id = lr.query_id().to_string();

        // Resolve symbol to SymbolId
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

        // Ensure structural data is available (may trigger lazy extraction)
        if let Ok(Some(sym)) = self.project().store.find_symbol_by_id(&sid) {
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
        let store = self.project().store.clone();
        let (cfg_nodes, cfg_edges) = match self
            .project()
            .analysis_runtime
            .ensure_cfg_for_function(&store, &sid, &query_id, &symbol)
        {
            Ok((nodes, edges)) => (nodes, edges),
            Err(e) => {
                let resp = json!({
                    "ok": false,
                    "function": symbol,
                    "field_path": field,
                    "error": e,
                });
                return lr.with_is_error(true).build(resp, self);
            }
        };

        // --- CFG is available — run lifecycle analysis ---

        // Lifecycle analysis only supports C/C++ — gate on language
        let sym_info = self
            .project()
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .and_then(|s| {
                let lang = match s.language {
                    atlas_engine::Language::C => Some("c"),
                    atlas_engine::Language::Cpp => Some("cpp"),
                    _ => None,
                };
                lang.map(|l| (s.qualified_name, l))
            });

        let (qname, lang_str) = match sym_info {
            Some((qname, lang)) => (qname, lang),
            None => {
                let resp = json!({
                    "ok": false,
                    "function": symbol,
                    "field_path": field,
                    "error": "unsupported_language",
                    "message": "Lifecycle analysis only supports C/C++. The requested symbol is not C/C++ or could not be resolved.",
                    "verdict": "incomplete",
                });
                return lr.with_is_error(true).build(resp, self);
            }
        };

        // Load domain rules from DB for this symbol's language
        let cpp_rules =
            atlas_engine::analysis::CppOwnershipRules::load_for(&self.project().store, lang_str);
        let has_any_rules = cpp_rules.has_any_rules();
        let has_user_rules = cpp_rules.has_user_rules();
        let ownership_rules = atlas_engine::analysis::OwnershipRules::default();

        // Run rule-backed lifecycle analysis
        let mut result = atlas_engine::analysis::FieldLifecycleEngine::analyze_with_rules(
            &cfg_nodes,
            &cfg_edges,
            &field,
            &ownership_rules,
            &cpp_rules,
        );
        result.function_qname = qname;

        // Build proof from lifecycle result
        let proof = atlas_engine::analysis::evaluate_proof(
            &result.suspicious_points,
            result.final_state,
            has_user_rules,
            has_any_rules,
        );

        // Build proof_paths from transitions
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
            "partial": result.partial,
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

        if result.partial {
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_unit("function".into())
                .with_analysis_coverage("boundary_partial".into())
                .with_analysis_basis(vec!["cfg".into(), "domain_rules".into()])
                .with_analysis_summary(format!(
                    "Partial lifecycle analysis: {} transitions found, final state={}, {} suspicious point(s).",
                    result.transitions.len(),
                    result.final_state.as_str(),
                    result.suspicious_points.len(),
                ))
                .with_analysis_retry_after_ms(8000);
        } else {
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_unit("function".into())
                .with_analysis_coverage("function_complete".into())
                .with_analysis_basis(vec!["cfg".into(), "domain_rules".into()])
                .with_gap_records(vec![])
                .with_analysis_summary("Lifecycle analysis used CFG and domain rules.".into());
        }
        lr.with_is_error(false).build(resp, self)
    }
}

/// Build specific gap descriptions from the lifecycle result.
/// Each gap identifies WHAT is actually missing in the partial analysis.
fn build_lifecycle_gaps(result: &atlas_engine::analysis::FieldLifecycleResult) -> Vec<GapRecord> {
    let scope = format!("function {}", result.function_qname);
    let mut gaps: Vec<GapRecord> = Vec::new();
    if result.transitions.is_empty() {
        gaps.push(GapRecord {
            scope: scope.clone(),
            reason: "no_transitions".into(),
            detail: "No state transitions were found for the given field path. The field may not participate in any allocation/free operations within this function.".into(),
        });
    }
    if result.final_state == atlas_engine::analysis::FieldState::Unknown {
        gaps.push(GapRecord {
            scope: scope.clone(),
            reason: "incomplete_cfg".into(),
            detail: "The final state of the field could not be determined. The CFG path may be incomplete due to unresolved calls or missing dataflow facts.".into(),
        });
    }
    if result.suspicious_points.is_empty() {
        gaps.push(GapRecord {
            scope: scope.clone(),
            reason: "no_domain_rules".into(),
            detail: "No ownership-rule matches were applied. Domain rules for this language/pattern may need to be configured.".into(),
        });
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::analysis::{
        EvidenceLevel, FieldLifecycleResult, FieldState, FieldTransition, SuspiciousPoint,
    };

    #[test]
    fn gaps_when_transitions_empty_contains_lifecycle_transitions() {
        let result = FieldLifecycleResult {
            field_path: "ptr".into(),
            function_qname: "foo".into(),
            transitions: vec![],
            final_state: FieldState::Assigned,
            suspicious_points: vec![],
            partial: true,
            evidence_level: EvidenceLevel::Incomplete,
            exit_state: None,
        };
        let gaps = build_lifecycle_gaps(&result);
        assert!(
            gaps.iter().any(|g| g.reason == "no_transitions"),
            "expected no_transitions gap, got: {gaps:?}"
        );
        assert!(
            gaps[0].scope.contains("foo"),
            "expected scope to contain function name, got: {gaps:?}"
        );
    }

    #[test]
    fn gaps_when_final_state_unknown_contains_lifecycle_end_state() {
        let result = FieldLifecycleResult {
            field_path: "ptr".into(),
            function_qname: "foo".into(),
            transitions: vec![],
            final_state: FieldState::Unknown,
            suspicious_points: vec![],
            partial: true,
            evidence_level: EvidenceLevel::Incomplete,
            exit_state: None,
        };
        let gaps = build_lifecycle_gaps(&result);
        assert!(
            gaps.iter().any(|g| g.reason == "incomplete_cfg"),
            "expected incomplete_cfg gap, got: {gaps:?}"
        );
    }

    #[test]
    fn gaps_when_suspicious_points_empty_contains_lifecycle_analysis() {
        let result = FieldLifecycleResult {
            field_path: "ptr".into(),
            function_qname: "foo".into(),
            transitions: vec![],
            final_state: FieldState::Assigned,
            suspicious_points: vec![],
            partial: true,
            evidence_level: EvidenceLevel::Incomplete,
            exit_state: None,
        };
        let gaps = build_lifecycle_gaps(&result);
        assert!(
            gaps.iter().any(|g| g.reason == "no_domain_rules"),
            "expected no_domain_rules gap, got: {gaps:?}"
        );
    }

    #[test]
    fn gaps_when_all_present_contains_all_three() {
        let result = FieldLifecycleResult {
            field_path: "ptr".into(),
            function_qname: "foo".into(),
            transitions: vec![],
            final_state: FieldState::Unknown,
            suspicious_points: vec![],
            partial: true,
            evidence_level: EvidenceLevel::Incomplete,
            exit_state: None,
        };
        let gaps = build_lifecycle_gaps(&result);
        assert_eq!(gaps.len(), 3, "expected all three gaps, got: {gaps:?}");
        assert!(gaps.iter().any(|g| g.reason == "no_transitions"));
        assert!(gaps.iter().any(|g| g.reason == "incomplete_cfg"));
        assert!(gaps.iter().any(|g| g.reason == "no_domain_rules"));
    }

    #[test]
    fn gaps_when_result_is_rich_produces_empty_gaps() {
        // When transitions exist, final_state is known, and suspicious_points exist,
        // there should be no gaps (analysis is as complete as it can be).
        let result = FieldLifecycleResult {
            field_path: "ptr".into(),
            function_qname: "foo".into(),
            transitions: vec![FieldTransition {
                from_state: FieldState::Unknown,
                to_state: FieldState::Assigned,
                node_id: atlas_engine::CfgNodeId::default(),
                node_line: 10,
                effect: None,
                branch_frames: vec![],
            }],
            final_state: FieldState::Assigned,
            suspicious_points: vec![SuspiciousPoint {
                line: 10,
                kind: atlas_engine::analysis::SuspiciousKind::MissingFree,
                message: "test".into(),
            }],
            partial: true,
            evidence_level: EvidenceLevel::Incomplete,
            exit_state: None,
        };
        let gaps = build_lifecycle_gaps(&result);
        assert!(gaps.is_empty(), "expected no gaps, got: {gaps:?}");
    }
}
