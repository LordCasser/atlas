//! lifecycle — field lifecycle analysis using CFG effect annotations.
//!
//! Given a function symbol and a field path, walks the function's CFG nodes
//! with effect annotations to produce a state-machine view of the field's
//! lifecycle: allocation, use, escape, free, and suspicious patterns (use-after-free, double-free).

use super::lazy_response::{LazyDiagnostics, LazyResponse};
use super::{MAX_SYMBOL_NAME_LENGTH, ToolRouter, get_str};
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
        if symbol.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }
        let field_raw = get_str(args, "field");
        let field = atlas_engine::canonicalize_field_path(field_raw);

        if symbol.is_empty() || field.is_empty() {
            return (
                "Missing required parameters: symbol and field".to_string(),
                true,
            );
        }

        let lr = LazyResponse::new("lifecycle", args);
        let query_id = lr.query_id().to_string();

        // Resolve symbol to SymbolId
        let sid = match self.resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle) {
            Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                let candidates_str: Vec<String> = candidates
                    .iter()
                    .take(5)
                    .map(|c| format!("{}::{} [{}]", c.file_path, c.line, c.kind))
                    .collect();
                return (
                    format!(
                        "Symbol '{}' is ambiguous ({} matches: {}). Use a SymbolSelector object from search results (symbol_ref field).",
                        symbol,
                        candidates.len(),
                        candidates_str.join(", ")
                    ),
                    true,
                );
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                let mut err = format!("Symbol not found: {symbol}");
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
            Err(e) => return (e, true),
        };

        // Ensure structural data is available (may trigger lazy extraction)
        if let Ok(Some(sym)) = self.store.find_symbol_by_id(&sid) {
            let (roots, _warnings) = self.include_roots_from_args(args);
            let _ = self.ensure_structural_for_files([sym.file_id], roots, None, Some(&query_id));
        }

        // Load CFG nodes for this function, with lazy CFG fallback
        let mut cfg_nodes = match self.store.find_cfg_nodes_by_function(&sid) {
            Ok(nodes) => nodes,
            Err(e) => return (format!("Failed to load CFG nodes: {e}"), true),
        };
        let mut cfg_edges = self
            .store
            .find_cfg_edges_by_function(&sid)
            .unwrap_or_default();

        let mut lazy_window: Option<atlas_engine::LazyWindow> = None;

        if cfg_nodes.is_empty() {
            // Trigger lazy CFG extraction via the dataflow service
            match self.lazy_service.ensure_for_function(&sid, Some(&query_id)) {
                Ok(window) => {
                    lazy_window = Some(window);
                    // Re-query CFG after lazy extraction
                    cfg_nodes = match self.store.find_cfg_nodes_by_function(&sid) {
                        Ok(nodes) => nodes,
                        Err(e) => {
                            return (
                                format!("Failed to load CFG nodes after lazy extraction: {e}"),
                                true,
                            );
                        }
                    };
                    cfg_edges = self
                        .store
                        .find_cfg_edges_by_function(&sid)
                        .unwrap_or_default();
                }
                Err(e) => {
                    // Lazy extraction itself failed — return graceful diagnostics
                    let diagnostics = LazyDiagnostics::from_layers(None, None, None);
                    let resp = json!({
                        "ok": false,
                        "function": symbol,
                        "field_path": field,
                        "error": format!("CFG not available for lifecycle analysis: {:#}", e),
                    });
                    return lr.with_is_error(true)
                        .with_lazy_diag(diagnostics)
                        .build(resp, self);
                }
            }
        }

        // After all attempts, if CFG is still unavailable, return graceful diagnostics
        if cfg_nodes.is_empty() {
            let diagnostics = LazyDiagnostics::from_layers(None, lazy_window.as_ref(), None);
            let resp = json!({
                "ok": false,
                "function": symbol,
                "field_path": field,
                "message": "CFG not available for lifecycle analysis. The function may be in a language that does not yet support CFG extraction, or the source file could not be read. Consider running 'index' with full structural analysis first.",
            });
            return lr.with_is_error(true)
                .with_lazy_diag(diagnostics)
                .build(resp, self);
        }

        // --- CFG is available — run lifecycle analysis ---

        // Lifecycle analysis only supports C/C++ — gate on language
        let sym_info = self
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
        let cpp_rules = atlas_engine::analysis::CppOwnershipRules::load_for(&self.store, lang_str);
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

        // Build lazy_diag from lazy_window if dataflow was triggered
        let lazy_diag: Option<LazyDiagnostics> = lazy_window
            .as_ref()
            .and_then(|window| LazyDiagnostics::from_layers(None, Some(window), None));

        lr.with_is_error(false)
            .with_lazy_diag(lazy_diag)
            .build(resp, self)
    }
}
