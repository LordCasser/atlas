//! lifecycle — field lifecycle analysis using CFG effect annotations.
//!
//! Given a function symbol and a field path, walks the function's CFG nodes
//! with effect annotations to produce a state-machine view of the field's
//! lifecycle: allocation, use, escape, free, and suspicious patterns (use-after-free, double-free).

use super::analysis_envelope::AnalysisEnvelope;
use super::{get_str, ToolRouter};
use crate::tools::symbol_selector::{
    parse_symbol_input, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_lifecycle(&mut self, args: &serde_json::Value) -> (String, bool) {
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
        let sid = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle)
        {
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

        // Ensure structural data is available (may trigger lazy extraction)
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
                    "field_path": field,
                    "error": e,
                });
                return lr.with_is_error(true).build(resp, self);
            }
        };

        // --- CFG is available — run lifecycle analysis ---

        // Lifecycle analysis only supports C/C++ — gate on language
        let sym_info = self
            .active_mut()
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
            atlas_engine::analysis::CppOwnershipRules::load_for(&self.active_mut().store, lang_str);
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
                .with_analysis_state("boundary".into())
                .with_analysis_scope("local".into())
                .with_analysis_unit("function".into())
                .with_analysis_coverage("boundary_partial".into())
                .with_analysis_basis(vec!["cfg".into(), "domain_rules".into()])
                .with_analysis_missing(vec!["complete_lifecycle_path".into()])
                .with_analysis_summary(
                    "Lifecycle analysis used CFG and domain rules, but the lifecycle path is incomplete."
                        .into(),
                )
                .with_analysis_next_action("wait_then_resume".into())
                .with_analysis_retry_after_ms(2000)
                .with_partial_result(true);
        } else {
            lr = lr
                .with_analysis_state("ready".into())
                .with_analysis_scope("local".into())
                .with_analysis_unit("function".into())
                .with_analysis_coverage("function_complete".into())
                .with_analysis_basis(vec!["cfg".into(), "domain_rules".into()])
                .with_analysis_missing(vec![])
                .with_analysis_summary("Lifecycle analysis used CFG and domain rules.".into())
                .with_analysis_next_action("use_result".into());
        }
        lr.with_is_error(false).build(resp, self)
    }
}
