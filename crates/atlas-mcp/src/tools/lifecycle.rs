//! atlas_lifecycle — field lifecycle analysis using CFG effect annotations.
//!
//! Given a function symbol and a field path, walks the function's CFG nodes
//! with effect annotations to produce a state-machine view of the field's
//! lifecycle: allocation, use, escape, free, and suspicious patterns (use-after-free, double-free).

use super::{ToolRouter, get_str};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_atlas_lifecycle(&mut self, args: &serde_json::Value) -> (String, bool) {
        let symbol = get_str(args, "symbol");
        let field = get_str(args, "field");

        if symbol.is_empty() || field.is_empty() {
            return ("Missing required parameters: symbol and field".to_string(), true);
        }

        // Resolve symbol to SymbolId
        let sid = match self.resolve_qname(symbol) {
            Ok(id) => id,
            Err(e) => return (e, true),
        };

        // Ensure structural data is available (may trigger lazy extraction)
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
                format!("No CFG nodes found for symbol '{}'. CFG is required for lifecycle analysis. Try re-indexing with --analysis full or trigger a trace query first.", symbol),
                true,
            );
        }

        // Get function qualified name for response
        let qname = self
            .store
            .find_symbol_by_id(&sid)
            .ok()
            .flatten()
            .map(|s| s.qualified_name)
            .unwrap_or_else(|| symbol.to_string());

        // Run lifecycle analysis
        let rules = atlas_engine::analysis::OwnershipRules::default();
        let mut result = atlas_engine::analysis::FieldLifecycleEngine::analyze_field_lifecycle(
            &cfg_nodes, field, &rules,
        );
        result.function_qname = qname;

        let resp = json!({
            "ok": true,
            "field_path": result.field_path,
            "function": result.function_qname,
            "final_state": result.final_state.as_str(),
            "transitions_count": result.transitions.len(),
            "transitions": result.transitions.iter().map(|t| json!({
                "from": t.from_state.as_str(),
                "to": t.to_state.as_str(),
                "line": t.line,
                "reason": t.reason,
            })).collect::<Vec<_>>(),
            "suspicious_count": result.suspicious_points.len(),
            "suspicious": result.suspicious_points.iter().map(|p| json!({
                "line": p.line,
                "kind": format!("{:?}", p.kind),
                "message": p.message,
            })).collect::<Vec<_>>(),
        });

        (
            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
