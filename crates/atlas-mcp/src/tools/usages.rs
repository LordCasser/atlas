//! `usages` — find all reference usages of a symbol.

use std::time::Instant;

use atlas_engine::InvestigationFocus;

use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{ToolRouter, get_u64};
use crate::tools::symbol_selector::{SymbolInput, SymbolResolution, SymbolResolutionPolicy};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_usages(&mut self, args: &serde_json::Value) -> (String, bool) {
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        // Unified symbol resolution via SymbolInput (string or structured selector).
        let input: SymbolInput = match serde_json::from_value(args["symbol"].clone()) {
            Ok(inp) => inp,
            Err(e) => return (format!("Invalid symbol parameter: {e}"), true),
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
                // BestEffortSingle with plain Name input: pick the first candidate.
                let first = &candidates[0];
                match self.store.find_symbols_by_qname(&first.qualified_name) {
                    Ok(symbols) => match symbols.first() {
                        Some(s) => s.id,
                        None => {
                            return (
                                format!(
                                    "Symbol '{}' found in candidates but not in store",
                                    first.qualified_name
                                ),
                                true,
                            );
                        }
                    },
                    Err(e) => return (format!("Lookup error: {e}"), true),
                }
            }
            SymbolResolution::NotFound { qname, .. } => {
                return (format!("Symbol not found: {qname}"), true);
            }
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

        // Use resolved qualified name from the input for the "symbol" field
        let symbol_display = match &input {
            SymbolInput::Name(s) => s.clone(),
            SymbolInput::Selector(sel) => sel.qualified_name.clone(),
        };
        let mut resp = json!({
            "symbol": symbol_display,
            "total_usages": refs.len(),
            "usages": usages,
        });

        let mut stored_args = args.clone();
        if let Some(obj) = stored_args.as_object_mut() {
            obj.insert("view".into(), serde_json::Value::String("usages".into()));
        }
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "symbol".into(),
            tool_args: stored_args,
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
