//! `dependents` — find files that import a given file.

use super::{ToolRouter, get_str, get_u64};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_dependents(&self, args: &serde_json::Value) -> (String, bool) {
        let file_id_hex = get_str(args, "file_id");
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        let fid = match file_id_hex.parse() {
            Ok(id) => id,
            Err(_) => return (format!("Invalid file_id: {file_id_hex}"), true),
        };

        let deps = match self.project().store.find_dependents_by_file(&fid) {
            Ok(d) => d,
            Err(e) => return (format!("Failed to query dependents: {e}"), true),
        };

        let shown = deps.iter().take(limit.min(100));
        let dependents: Vec<_> = shown
            .map(|(file_path, import_module)| {
                json!({
                    "file": file_path,
                    "import": import_module,
                })
            })
            .collect();

        (
            serde_json::to_string_pretty(&json!({
                "file": self.project().store_query_runtime.resolve_file_path(&fid),
                "total_dependents": deps.len(),
                "dependents": dependents,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
