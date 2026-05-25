//! `dependencies` — find files imported by a given file.

use super::{ToolRouter, get_str, get_u64};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_dependencies(&self, args: &serde_json::Value) -> (String, bool) {
        let file_id_hex = get_str(args, "file_id");
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        let fid = match file_id_hex.parse() {
            Ok(id) => id,
            Err(_) => return (format!("Invalid file_id: {file_id_hex}"), true),
        };

        let imports = match self.store.find_imports_by_file(&fid) {
            Ok(i) => i,
            Err(e) => return (format!("Failed to query imports: {e}"), true),
        };

        let shown = imports.iter().take(limit.min(100));
        let deps: Vec<_> = shown
            .map(|i| {
                json!({
                    "module": i.module,
                    "imported_name": i.imported_name,
                    "kind": i.kind.as_str(),
                })
            })
            .collect();

        (
            serde_json::to_string_pretty(&json!({
                "file": self.resolve_file_path(&fid),
                "file_id": file_id_hex,
                "total_dependencies": imports.len(),
                "dependencies": deps,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
