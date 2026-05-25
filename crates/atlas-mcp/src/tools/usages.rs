//! `usages` — find all reference usages of a symbol.

use super::{ToolRouter, get_str, get_u64};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_usages(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        // Accept hex SymbolId or qualified name
        let sid = match qname.parse() {
            Ok(id) => id,
            Err(_) => match self.resolve_qname(qname) {
                Ok(id) => id,
                Err(e) => return (e, true),
            },
        };

        let refs = match self.store.find_references_by_symbol(&sid) {
            Ok(r) => r,
            Err(e) => return (format!("Failed to query usages: {e}"), true),
        };

        let shown = refs.iter().take(limit.min(100));
        let usages: Vec<_> = shown
            .map(|r| {
                json!({
                    "text": r.text,
                    "kind": r.kind.as_str(),
                    "file": self.resolve_file_path(&r.file_id),
                    "file_id": r.file_id.to_hex(),
                    "line": r.range.start_line + 1,
                    "column": r.range.start_column + 1,
                })
            })
            .collect();

        (
            serde_json::to_string_pretty(&json!({
                "symbol": qname,
                "total_usages": refs.len(),
                "usages": usages,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
