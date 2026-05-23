//! Search tools: full-text symbol search and single-symbol lookup.

use atlas_engine::SymbolKind;

use super::{ToolRouter, get_str, get_str_opt, get_u64};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_search(&self, args: &serde_json::Value) -> (String, bool) {
        let query = get_str(args, "query");
        let limit = (get_u64(args, "limit").unwrap_or(20) as usize).min(200);
        let kind = get_str_opt(args, "kind");

        let results = if let Some(k_str) = kind {
            match SymbolKind::from_str(k_str) {
                Some(k) => self.search.search_by_kind(query, k, limit),
                None => return (format!("Unknown symbol kind: {}", k_str), true),
            }
        } else {
            self.search.search_simple(query, limit)
        };

        match results {
            Ok(entries) => (
                serde_json::to_string_pretty(&json!({
                    "query": query,
                    "count": entries.len(),
                    "results": entries.iter().map(|e| json!({
                        "name": e.symbol.name,
                        "qualified_name": e.symbol.qualified_name,
                        "kind": e.symbol.kind.as_str(),
                        "language": e.symbol.language.as_str(),
                        "score": e.score.total,
                        "file": e.symbol.file_id.to_hex(),
                    })).collect::<Vec<_>>(),
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
            Err(e) => (format!("Search error: {}", e), true),
        }
    }

    pub(crate) fn handle_symbol(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "qualified_name");
        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => return (format!("Lookup error: {}", e), true),
        };
        let sym = match symbols.first() {
            Some(s) => s,
            None => return (format!("Symbol not found: {}", qname), true),
        };

        let graph = self.search.graph_snapshot();
        let callers_count = graph.callers(&sym.id).callers.len();
        let callees_count = graph.callees(&sym.id).callees.len();

        (
            serde_json::to_string_pretty(&json!({
                "name": sym.name,
                "qualified_name": sym.qualified_name,
                "kind": sym.kind.as_str(),
                "language": sym.language.as_str(),
                "visibility": sym.visibility.as_ref().map(|v| v.as_str()),
                "signature": sym.signature,
                "file": sym.file_id.to_hex(),
                "range": {
                    "line": sym.range.start_line,
                    "column": sym.range.start_column,
                },
                "callers": callers_count,
                "callees": callees_count,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
