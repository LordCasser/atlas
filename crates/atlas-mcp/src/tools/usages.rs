//! `usages` — find all reference usages of a symbol.

use atlas_engine::InvestigationFocus;

use super::lazy_response::LazyResponse;
use super::{ToolRouter, get_u64};
use crate::tools::symbol_selector::{SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_usages(&mut self, args: &serde_json::Value) -> (String, bool) {
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        // Unified symbol resolution via SymbolInput (string or structured selector).
        let input: SymbolInput = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let resolution = match self
            .resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle)
        {
            Ok(r) => r,
            Err(e) => return (e, true),
        };
        let sid = match resolution {
            SymbolResolution::Single { symbol_id, .. } => symbol_id,
            SymbolResolution::Ambiguous { candidates, .. } => {
                candidates[0].symbol_id
            }
            SymbolResolution::NotFound { qname, .. } => {
                return (format!("Symbol not found: {qname}"), true);
            }
        };

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let _investigation = self.active.job_runtime.investigation_state.active_investigation.clone();

        let lr = LazyResponse::new("symbol", args);

        let refs = match self.active.store.find_references_by_symbol(&sid) {
            Ok(r) => r,
            Err(e) => return (format!("Failed to query usages: {e}"), true),
        };

        let shown = refs.iter().take(limit.min(100));
        let usages: Vec<_> = shown
            .map(|r| {
                let mask = self
                    .active.store
                    .get_capability_mask(&r.file_id)
                    .unwrap_or_default();
                json!({
                    "text": r.text,
                    "kind": r.kind.as_str(),
                    "file": self.resolve_file_path(&r.file_id),
                    "line": r.range.start_line + 1,
                    "column": r.range.start_column + 1,
                    "evidence_level": mask.best_capability_name(),
                    "source_capability": mask.bits(),
                })
            })
            .collect();

        // Use resolved qualified name from the input for the "symbol" field
        let symbol_display = match &input {
            SymbolInput::Name(s) => s.clone(),
            SymbolInput::Selector(sel) => sel.qualified_name.clone(),
        };
        let resp = json!({
            "symbol": symbol_display,
            "total_usages": refs.len(),
            "usages": usages,
        });

        let mut stored_args = args.clone();
        if let Some(obj) = stored_args.as_object_mut() {
            obj.insert("view".into(), serde_json::Value::String("usages".into()));
        }

        lr.with_is_error(false)
            .build_with_args(resp, &stored_args, self)
    }
}
