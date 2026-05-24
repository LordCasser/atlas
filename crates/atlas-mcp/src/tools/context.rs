//! Context tool: builds rich markdown context for a symbol.

use super::{ToolRouter, get_str};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_context(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => return (format!("Lookup error: {}", e), true),
        };
        let sid = match symbols.first().map(|s| s.id) {
            Some(id) => id,
            None => return (format!("Symbol not found: {}", qname), true),
        };
        match self.context_builder().build_context_for_symbol(&sid) {
            Ok(view) => {
                // Wrap markdown in JSON so it's not misdetected as error
                let md = view.to_markdown();
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
