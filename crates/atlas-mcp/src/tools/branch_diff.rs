//! branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.

use super::lazy_response::LazyDiagnostics;
use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{MAX_SYMBOL_NAME_LENGTH, QnameResolution, ToolRouter, get_str};
use serde_json::json;
use std::time::Instant;

impl ToolRouter {
    pub(crate) fn handle_branch_diff(&mut self, args: &serde_json::Value) -> (String, bool) {
        let symbol = get_str(args, "symbol");
        if symbol.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!("symbol exceeds max length of {MAX_SYMBOL_NAME_LENGTH}"),
                true,
            );
        }

        if symbol.is_empty() {
            return ("Missing required parameter: symbol".to_string(), true);
        }

        // Resolve symbol to SymbolId
        let sid = match self.resolve_qname_disambiguated(symbol) {
            Ok(QnameResolution::Unique(id)) => id,
            Ok(QnameResolution::Ambiguous { candidates }) => {
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
            Err(e) => return (e, true),
        };

        // Generate query_id for resume / tasks
        let query_id = Self::generate_query_id();

        // Ensure structural data is available
        if let Ok(Some(sym)) = self.store.find_symbol_by_id(&sid) {
            let (roots, _warnings) = self.include_roots_from_args(args);
            let _ = self.ensure_structural_for_files([sym.file_id], roots, None, Some(&query_id));
        }

        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "branch_diff".into(),
            tool_args: args.clone(),
            lazy_window: None,
            created_at: Instant::now(),
            status: QueryStatus::Partial,
        });

        // Load CFG nodes for this function, with lazy CFG fallback
        let mut cfg_nodes = match self.store.find_cfg_nodes_by_function(&sid) {
            Ok(nodes) => nodes,
            Err(e) => return (format!("Failed to load CFG nodes: {e}"), true),
        };

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
                }
                Err(e) => {
                    // Lazy extraction itself failed — return graceful diagnostics
                    let diagnostics = LazyDiagnostics::from_layers(None, None, None);
                    let resp = json!({
                        "ok": false,
                        "function": symbol,
                        "error": format!("CFG not available for branch diff analysis: {:#}", e),
                        "lazy_diagnostics": diagnostics,
                    });
                    return (
                        serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                        false,
                    );
                }
            }
        }

        // After all attempts, if CFG is still unavailable, return graceful diagnostics
        if cfg_nodes.is_empty() {
            let diagnostics = LazyDiagnostics::from_layers(None, lazy_window.as_ref(), None);
            let resp = json!({
                "ok": false,
                "function": symbol,
                "message": "CFG not available for branch diff analysis. The function may be in a language that does not yet support CFG extraction, or the source file could not be read. Consider running 'index' with full structural analysis first.",
                "lazy_diagnostics": diagnostics,
            });
            return (
                serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                false,
            );
        }

        // --- CFG is available — run branch diff analysis ---

        let qname = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.qualified_name)
            .unwrap_or_else(|| symbol.to_string());

        let cfg_edges = self
            .store
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
                .store
                .find_symbol_by_id(&sid)
                .ok()
                .flatten()
                .map(|s| s.language)
                .unwrap_or(atlas_engine::Language::C);
            let contract = atlas_engine::analysis::ResourceOpConfig::default_for(lang);

            // Load DataFlow nodes and edges
            let data_nodes = self
                .store
                .find_data_nodes_by_function(&sid)
                .unwrap_or_default();
            let dataflow_edges = if data_nodes.is_empty() {
                vec![]
            } else {
                let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
                self.store
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

        let mut resp = json!({
            "ok": true,
            "query_id": query_id,
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

        // Attach lazy diagnostics if dataflow was triggered
        if let Some(ref window) = lazy_window {
            let diagnostics = LazyDiagnostics::from_layers(None, Some(window), None);
            if let Some(diag) = diagnostics {
                resp["lazy_diagnostics"] = json!(diag);
            }
        }

        (
            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
