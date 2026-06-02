//! `usages` — find all reference usages of a symbol.

use std::time::Instant;

use atlas_engine::InvestigationFocus;

use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{ToolRouter, get_str, get_u64};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_usages(&mut self, args: &serde_json::Value) -> (String, bool) {
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

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let _investigation = self.investigation_state.active_investigation.clone();
        let query_id = Self::generate_query_id();

        let refs = match self.store.find_references_by_symbol(&sid) {
            Ok(r) => r,
            Err(e) => return (format!("Failed to query usages: {e}"), true),
        };

        let shown = refs.iter().take(limit.min(100));
        let usages: Vec<_> = shown
            .map(|r| {
                let mask = self
                    .store
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

        let mut resp = json!({
            "symbol": qname,
            "total_usages": refs.len(),
            "usages": usages,
        });

        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "usages".into(),
            tool_args: args.clone(),
            lazy_window: None,
            created_at: Instant::now(),
            status: QueryStatus::Ready,
        });
        resp["query_id"] = json!(query_id);

        (
            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
