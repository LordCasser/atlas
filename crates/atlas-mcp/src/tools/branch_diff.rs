//! atlas_branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.

use super::lazy_response::LazyDiagnostics;
use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{ToolRouter, get_str};
use serde_json::json;
use std::time::Instant;

impl ToolRouter {
    pub(crate) fn handle_atlas_branch_diff(&mut self, args: &serde_json::Value) -> (String, bool) {
        let symbol = get_str(args, "symbol");

        if symbol.is_empty() {
            return ("Missing required parameter: symbol".to_string(), true);
        }

        // Resolve symbol to SymbolId
        let sid = match self.resolve_qname(symbol) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        // Generate query_id for atlas_resume / atlas_jobs
        let query_id = Self::generate_query_id();

        // Ensure structural data is available
        if let Ok(Some(sym)) = self.store.find_symbol_by_id(&sid) {
            let (roots, _warnings) = self.include_roots_from_args(args);
            let _ = self.ensure_structural_for_files([sym.file_id], roots, None, Some(&query_id));
        }

        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "atlas_branch_diff".into(),
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

        // Branch diff only supports C/C++ — gate on language
        let is_c_or_cpp = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| {
                matches!(
                    s.language,
                    atlas_engine::Language::C | atlas_engine::Language::Cpp
                )
            })
            .unwrap_or(false);

        if !is_c_or_cpp {
            let resp = json!({
                "ok": false,
                "function": symbol,
                "error": "unsupported_language",
                "message": "Branch diff analysis only supports C/C++",
            });
            return (
                serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                false,
            );
        }

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
        let diffs = atlas_engine::analysis::BranchDiffEngine::diff_branches(&cfg_nodes, &cfg_edges);

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
