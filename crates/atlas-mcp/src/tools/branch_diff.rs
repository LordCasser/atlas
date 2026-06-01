//! atlas_branch_diff — branch side-effect comparison using CFG effect annotations.
//!
//! Compares the side effects of sibling branch paths (if/else, switch cases)
//! within a function. Detects suspicious asymmetries like one branch freeing
//! a field while the other does not.

use super::{ToolRouter, get_str};
use serde_json::json;

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

        // Ensure structural data is available
        if let Ok(Some(sym)) = self.store.find_symbol_by_id(&sid) {
            let (roots, _warnings) = self.include_roots_from_args(args);
            let _ = self.ensure_structural_for_files([sym.file_id], roots, None, None);
        }

        // Load CFG nodes for this function
        let cfg_nodes = match self.store.find_cfg_nodes_by_function(&sid) {
            Ok(nodes) => nodes,
            Err(e) => return (format!("Failed to load CFG nodes: {e}"), true),
        };

        if cfg_nodes.is_empty() {
            return (
                format!("No CFG nodes found for symbol '{}'. CFG is required for branch analysis. Try re-indexing with --analysis full or trigger a trace query first.", symbol),
                true,
            );
        }

        let qname = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.qualified_name)
            .unwrap_or_else(|| symbol.to_string());

        let diffs = atlas_engine::analysis::BranchDiffEngine::diff_branches(&cfg_nodes);

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

        (
            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
