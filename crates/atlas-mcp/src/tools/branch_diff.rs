//! branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.

use super::lazy_response::LazyResponse;
use super::{MAX_SYMBOL_NAME_LENGTH, ToolRouter};
use crate::tools::symbol_selector::{
    parse_symbol_input, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
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
        if symbol.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }

        if symbol.is_empty() {
            return ("Missing required parameter: symbol".to_string(), true);
        }

        let lr = LazyResponse::new("branch_diff", args);
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
                err.push_str(self.active_mut().store_query_runtime.not_indexed_guidance());
                return (err, true);
            }
            Err(e) => return (e, true),
        };

        // Ensure structural data is available
        if let Ok(Some(sym)) = self.active_mut().store.find_symbol_by_id(&sid) {
            let (_, focus_warnings) = self.prepare_focus_query(
                Some(atlas_engine::QueryIntent::Calls {
                    symbol_name: sym.name.clone(),
                    file_id: Some(sym.file_id),
                    symbol_id: None,
                    direction: None,
                    depth: None,
                }),
            );
            for w in focus_warnings {
                tracing::warn!("Focus pre-warm warning (branch_diff): {w}");
            }
        }

        // Load CFG nodes for this function, with lazy CFG fallback
        let mut cfg_nodes = match self.active_mut().store.find_cfg_nodes_by_function(&sid) {
            Ok(nodes) => nodes,
            Err(e) => return (format!("Failed to load CFG nodes: {e}"), true),
        };

        if cfg_nodes.is_empty() {
            // Trigger lazy CFG extraction via the dataflow service
            match self.active_mut().analysis_runtime.ensure_dataflow_for_function(&sid, Some(&query_id)) {
                Ok(()) => {
                    // Re-query CFG after lazy extraction
                    cfg_nodes = match self.active_mut().store.find_cfg_nodes_by_function(&sid) {
                        Ok(nodes) => nodes,
                        Err(e) => {
                            return (
                                format!("Failed to load CFG nodes after lazy extraction: {e}"),
                                true,
                            );
                        }
                    };
                }
                Err(e) => {
                    // Lazy extraction itself failed — return graceful diagnostics
                    let resp = json!({
                        "ok": false,
                        "function": symbol,
                        "error": format!("CFG not available for branch diff analysis: {:#}", e),
                    });
                    return lr.with_is_error(true)
                        .build(resp, self);
                }
            }
        }

        // After all attempts, if CFG is still unavailable, return graceful diagnostics
        if cfg_nodes.is_empty() {
            let resp = json!({
                "ok": false,
                "function": symbol,
                "message": "CFG not available for branch diff analysis. The function may be in a language that does not yet support CFG extraction, or the source file could not be read. Consider running 'index' with full structural analysis first.",
            });
            return lr.with_is_error(true)
                .build(resp, self);
        }

        // --- CFG is available — run branch diff analysis ---

        let qname = self
            .active_mut().store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.qualified_name)
            .unwrap_or_else(|| symbol.to_string());

        let cfg_edges = self
            .active_mut().store
            .find_cfg_edges_by_function(&sid)
            .unwrap_or_default();

        // Check for semantic analysis mode (default: true)
        let use_semantic = args
            .get("semantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let diffs = if use_semantic {
            // ── SEMANTIC PATH: compose_effects + diff_branches_semantic ──
            let lang = self
                .active_mut().store
                .find_symbol_by_id(&sid)
                .ok()
                .flatten()
                .map(|s| s.language)
                .unwrap_or(atlas_engine::Language::C);
            let contract = atlas_engine::analysis::ResourceOpConfig::default_for(lang);

            // Load DataFlow nodes and edges
            let data_nodes = self
                .active_mut().store
                .find_data_nodes_by_function(&sid)
                .unwrap_or_default();
            let dataflow_edges = if data_nodes.is_empty() {
                vec![]
            } else {
                let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
                self.active_mut().store
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

        lr.with_is_error(false)
            .build(resp, self)
    }
}
