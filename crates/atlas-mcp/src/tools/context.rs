//! Context tool: builds rich markdown context for a symbol.
//! Includes transparent lazy structural extraction with progress.

use atlas_engine::LazyStructuralService;

use super::{ToolRouter, get_str};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_context(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        self.send_progress(0.2, &format!("Building context for '{}'...", qname));

        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => {
                let mut err = format!("Lookup error: {}", e);
                err.push_str(self.index_not_run_guidance());
                return (err, true);
            }
        };
        let sid = match symbols.first().map(|s| s.id) {
            Some(id) => id,
            None => {
                // Try lazy structural extraction
                self.send_progress(0.5, "Extracting structural data...");
                let lazy =
                    LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
                let _ = lazy.ensure_structural_for_symbol(qname);
                // Re-query
                let retry = self.store.find_symbols_by_qname(qname).unwrap_or_default();
                match retry.first().map(|s| s.id) {
                    Some(id) => id,
                    None => {
                        let mut err = format!("Symbol not found: {}", qname);
                        err.push_str(self.index_not_run_guidance());
                        return (err, true);
                    }
                }
            }
        };

        self.send_progress(0.7, "Building context view...");
        match self.context_builder().build_context_for_symbol(&sid) {
            Ok(view) => {
                let md = view.to_markdown();
                self.send_progress(1.0, "Context complete");
                (
                    serde_json::to_string_pretty(&json!({
                        "markdown": md,
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            Err(e) => (format!("Context build error: {}", e), true),
        }
    }
}
