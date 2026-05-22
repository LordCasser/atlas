//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.

use atlas_analysis::trace::{TraceEngine, TraceQueryResponse};
use atlas_types::SymbolId;

use super::{ToolRouter, get_str_opt, get_u64, resolve_file_id};

impl ToolRouter {
    pub(crate) fn handle_trace_point(&self, args: &serde_json::Value) -> (String, bool) {
        let file_hex = get_str_opt(args, "file_id");
        let file_path = get_str_opt(args, "file_path");
        let line = get_u64(args, "line");
        let column = get_u64(args, "column");

        let file_id = match resolve_file_id(&self.store, &self.project_root, file_hex, file_path) {
            Ok(Some(fid)) => fid,
            Ok(None) => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_point", "Missing file_id or file_path");
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
            Err(e) => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_point", &format!("Error resolving file: {}", e));
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        let (line, column) = match (line, column) {
            (Some(l), Some(c)) => (l as u32, c as u32),
            _ => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_point", "Missing line or column");
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        let engine = TraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        let resp = engine.trace_point(&file_id, line, column);
        let is_error = !resp.ok;

        (
            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }

    pub(crate) fn handle_trace_variable(&self, args: &serde_json::Value) -> (String, bool) {
        let file_hex = get_str_opt(args, "file_id");
        let file_path = get_str_opt(args, "file_path");
        let line = get_u64(args, "line");
        let column = get_u64(args, "column");
        let max_depth = get_u64(args, "max_depth").unwrap_or(30) as usize;

        let file_id = match resolve_file_id(&self.store, &self.project_root, file_hex, file_path) {
            Ok(Some(fid)) => fid,
            Ok(None) => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_variable", "Missing file_id or file_path");
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_variable",
                    &format!("Error resolving file: {}", e),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        let (line, column) = match (line, column) {
            (Some(l), Some(c)) => (l as u32, c as u32),
            _ => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_variable", "Missing line or column");
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        let engine = TraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        let resp = engine.trace_variable(&file_id, line, column, max_depth);
        let is_error = !resp.ok;

        (
            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }

    pub(crate) fn handle_trace_caller_path(&self, args: &serde_json::Value) -> (String, bool) {
        let symbol_hex = args["symbol"].as_str().filter(|s| !s.is_empty());
        let symbol_name = args["symbol_name"].as_str().filter(|s| !s.is_empty());
        let max_depth = args["max_depth"].as_u64().unwrap_or(20) as usize;

        let engine = TraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        let resp = if let Some(hex) = symbol_hex {
            let target_id: SymbolId = match hex.parse() {
                Ok(id) => id,
                Err(e) => {
                    let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                        "trace_callers",
                        &format!("Invalid symbol hex ID: {}", e),
                    );
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
            };
            engine.trace_callers(&target_id, max_depth)
        } else if let Some(name) = symbol_name {
            engine.trace_callers_by_name(name, max_depth)
        } else {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                "Must provide either 'symbol' (hex) or 'symbol_name'",
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        };
        let is_error = !resp.ok;

        (
            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }
}
