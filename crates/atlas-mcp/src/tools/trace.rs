//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.  Includes transparent lazy structural extraction with progress
//! notifications to prevent MCP timeout during on-demand extraction.

use atlas_engine::SymbolId;
use atlas_engine::{
    LazyStructuralService, LazySummary, RawTraceEngine, TraceDiagnostic, TraceQueryResponse,
};

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
                let msg = if file_hex.is_some() || file_path.is_some() {
                    if !self.has_indexed_files() {
                        "No files indexed yet. Please run the 'index' tool first to build the code index, then retry this query."
                    } else {
                        "File not found in index. Check that the file_id or file_path is correct and belongs to the indexed project."
                    }
                } else {
                    "Missing file_id or file_path"
                };
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_point", msg);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
            Err(e) => {
                let mut err_msg = format!("Error resolving file: {}", e);
                err_msg.push_str(self.index_not_run_guidance());
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_point", &err_msg);
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

        // Transparent lazy structural: ensure file has structural data before tracing
        self.send_progress(0.3, "Ensuring structural index...");
        {
            let lazy =
                LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
            let _ = lazy.ensure_structural_for_file(&file_id);
        }

        let engine = RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        self.send_progress(0.8, "Running trace point...");
        let resp = engine.trace_point(&file_id, line, column);
        self.send_progress(1.0, "Trace complete");
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
                let msg = if file_hex.is_some() || file_path.is_some() {
                    if !self.has_indexed_files() {
                        "No files indexed yet. Please run the 'index' tool first to build the code index, then retry this query."
                    } else {
                        "File not found in index. Check that the file_id or file_path is correct and belongs to the indexed project."
                    }
                } else {
                    "Missing file_id or file_path"
                };
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_variable", msg);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
            Err(e) => {
                let mut err_msg = format!("Error resolving file: {}", e);
                err_msg.push_str(self.index_not_run_guidance());
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_variable", &err_msg);
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

        // Transparent lazy structural: ensure file has structural data before dataflow
        self.send_progress(0.2, "Ensuring structural index...");
        {
            let lazy =
                LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
            let _ = lazy.ensure_structural_for_file(&file_id);
        }

        // Lazy-load dataflow before tracing, so Locator can find data nodes.
        let lazy_start = std::time::Instant::now();
        let mut partial = false;
        let mut lazy_diags: Vec<TraceDiagnostic> = Vec::new();
        let lazy_summary: Option<LazySummary>;
        match self
            .lazy_service
            .ensure_for_position(&file_id, line, column)
        {
            Ok(window) => {
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: window.units_built,
                    units_cached: window.units_cached,
                    truncated: window.truncated,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                });
                if window.truncated {
                    partial = true;
                    lazy_diags.push(
                        TraceDiagnostic::warning(
                            "Lazy dataflow reached its internal budget. Result is partial. For full offline coverage, run `atlas index --analysis full`."
                        ).with_code("lazy_dataflow_budget_exceeded")
                    );
                }
            }
            Err(e) => {
                partial = true;
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: 0,
                    units_cached: 0,
                    truncated: true,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                });
                lazy_diags.push(
                    TraceDiagnostic::warning(&format!("Lazy dataflow build failed: {e}"))
                        .with_code("lazy_dataflow_build_failed"),
                );
            }
        }

        let engine = RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        let mut resp = engine.trace_variable(&file_id, line, column, max_depth);
        resp.partial_result = resp.partial_result || partial;
        resp.diagnostics.extend(lazy_diags);
        if let Some(ref mut path) = resp.result {
            path.lazy_summary = lazy_summary;
        }
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

        let engine = RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
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
