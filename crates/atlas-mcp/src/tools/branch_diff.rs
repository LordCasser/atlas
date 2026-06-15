//! branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.

use super::analysis_envelope::AnalysisEnvelope;
use super::{ToolRouter};
use crate::tools::symbol_selector::{
    SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};
use serde_json::json;

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

        lr.with_is_error(false).build(resp, self)
    }
}
