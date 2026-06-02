//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.  Includes transparent lazy structural extraction with progress
//! notifications to prevent MCP timeout during on-demand extraction.

use std::time::Instant;

use atlas_engine::SymbolId;
use atlas_engine::{
    InvestigationFocus, LazySummary, RawTraceEngine, TraceDiagnostic, TraceQueryResponse,
};

use super::lazy_response::LazyDiagnostics;
use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{
    MAX_FILE_PATH_LENGTH, MAX_SYMBOL_NAME_LENGTH, ToolRouter, get_str_opt, get_u64,
    resolve_file_id, warnings_to_trace_diagnostics,
};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_trace_point(&mut self, args: &serde_json::Value) -> (String, bool) {
        let file_hex = get_str_opt(args, "file_id");
        let file_path = get_str_opt(args, "file_path");
        let line = get_u64(args, "line");
        let column = get_u64(args, "column");

        // Validate file_path length
        if let Some(fp) = file_path {
            if fp.len() > MAX_FILE_PATH_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_point",
                    &format!(
                        "file_path exceeds maximum length of {MAX_FILE_PATH_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }

        // Parse include_roots
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

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
                let mut err_msg = format!("Error resolving file: {e}");
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

        // Update investigation state with position focus
        self.update_investigation(InvestigationFocus::Position {
            file_id,
            line,
            col: column,
        });
        let investigation = self.investigation_state.active_investigation.clone();
        let query_id = Self::generate_query_id();

        // Ensure structural before tracing
        let outcome = self.ensure_structural_for_files(
            [file_id],
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        // Capture lazy diagnostics before outcome fields are moved.
        let lazy_diag: Option<LazyDiagnostics> = outcome
            .lazy_outcome
            .as_ref()
            .map(LazyDiagnostics::from_structural);

        let engine = RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        self.send_progress(0.8, "Running trace point...");
        let mut resp = engine.trace_point(&file_id, line, column);
        self.send_progress(1.0, "Trace complete");

        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !outcome.warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            outcome.warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let is_error = !resp.ok;

        let tier = outcome.precision_tier;
        let mut resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));
        resp_value["structural_precision_tier"] = serde_json::to_value(tier).unwrap_or(json!(null));
        if tier != atlas_engine::structs::precision::PrecisionTier::Exact {
            if let Some(hint) = atlas_engine::precision::next_action_structural(tier) {
                resp_value["structural_hint"] = json!(hint);
            }
        }
        if let Some(ref diag) = lazy_diag {
            resp_value["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
            resp_value["analysis_contract"] =
                serde_json::to_value(&diag.analysis_contract).unwrap_or(json!(null));
        }

        // Store query snapshot for potential atlas_resume
        let all_complete = tier == atlas_engine::structs::precision::PrecisionTier::Exact;
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "trace_point".into(),
            tool_args: args.clone(),
            lazy_window: None, // trace_point only triggers structural, not dataflow
            created_at: Instant::now(),
            status: if all_complete {
                QueryStatus::Ready
            } else {
                QueryStatus::Partial
            },
        });
        resp_value["query_id"] = json!(query_id);

        (
            serde_json::to_string_pretty(&resp_value).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }

    pub(crate) fn handle_trace_variable(&mut self, args: &serde_json::Value) -> (String, bool) {
        let file_hex = get_str_opt(args, "file_id");
        let file_path = get_str_opt(args, "file_path");
        let line = get_u64(args, "line");
        let column = get_u64(args, "column");
        let max_depth = get_u64(args, "max_depth").unwrap_or(30) as usize;

        // Validate file_path length
        if let Some(fp) = file_path {
            if fp.len() > MAX_FILE_PATH_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_variable",
                    &format!(
                        "file_path exceeds maximum length of {MAX_FILE_PATH_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }

        // Parse include_roots
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

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
                let mut err_msg = format!("Error resolving file: {e}");
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

        // Update investigation state with position focus
        self.update_investigation(InvestigationFocus::Position {
            file_id,
            line,
            col: column,
        });
        let investigation = self.investigation_state.active_investigation.clone();
        let query_id = Self::generate_query_id();

        // Ensure structural before tracing
        let outcome = self.ensure_structural_for_files(
            [file_id],
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        // Capture structural lazy outcome before fields are moved.
        let structural_lo = outcome.lazy_outcome.clone();

        // Lazy-load dataflow before tracing, so Locator can find data nodes.
        // Always trigger lazy dataflow extraction — the loader internally
        // checks for pre-built data via `count_data_nodes_for_unit` and skips
        // extraction when data already exists.
        let lazy_start = std::time::Instant::now();
        let mut partial = false;
        let mut lazy_diags: Vec<TraceDiagnostic> = Vec::new();
        let lazy_summary: Option<LazySummary>;
        let mut combined_lazy_diag: Option<LazyDiagnostics> = None;
        let mut lazy_window: Option<atlas_engine::LazyWindow> = None;
        match self
            .lazy_service
            .ensure_for_position(&file_id, line, column, Some(&query_id))
        {
            Ok(window) => {
                lazy_window = Some(window.clone());
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: window.units_built,
                    units_cached: window.units_cached,
                    units_pending: window.units_pending,
                    pending_job_ids: window.pending_job_ids.clone(),
                    truncated: window.truncated,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                    precision_tier: window.precision_tier,
                });
                // Build combined diagnostics from both layers.
                combined_lazy_diag =
                    Some(LazyDiagnostics::from_both(structural_lo.as_ref(), &window));
                if window.truncated {
                    partial = true;
                    lazy_diags.push(
                        TraceDiagnostic::warning(
                            "Lazy dataflow reached its internal budget. Result is partial. For full offline coverage, run `atlas index --analysis full`."
                        ).with_code("lazy_dataflow_budget_exceeded")
                    );
                }
                if window.units_pending > 0 {
                    partial = true;
                    lazy_diags.push(
                        TraceDiagnostic::warning(
                            "Lazy dataflow is already being built by another request. Result may be partial; retry after the reported pending job completes."
                        )
                        .with_code("lazy_dataflow_already_building"),
                    );
                }
            }
            Err(e) => {
                partial = true;
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: 0,
                    units_cached: 0,
                    units_pending: 0,
                    pending_job_ids: Vec::new(),
                    truncated: true,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                    precision_tier: None,
                });
                // Fall back to structural-only diagnostics when dataflow fails.
                if let Some(ref lo) = structural_lo {
                    combined_lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                }
                lazy_diags.push(
                    TraceDiagnostic::warning(&format!("Lazy dataflow build failed: {e}"))
                        .with_code("lazy_dataflow_build_failed"),
                );
            }
        }

        let engine = RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        let mut resp = engine.trace_variable(&file_id, line, column, max_depth);
        let lazy_partial = !outcome.warnings.is_empty();
        resp.partial_result = resp.partial_result || partial || lazy_partial;
        resp.diagnostics.extend(lazy_diags);
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            outcome.warnings,
            "lazy_structural_warning",
        ));
        if let Some(ref mut path) = resp.result {
            path.lazy_summary = lazy_summary;
        }
        let is_error = !resp.ok;

        let tier = outcome.precision_tier;
        let mut resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));
        resp_value["structural_precision_tier"] = serde_json::to_value(tier).unwrap_or(json!(null));
        if tier != atlas_engine::structs::precision::PrecisionTier::Exact {
            if let Some(hint) = atlas_engine::precision::next_action_structural(tier) {
                resp_value["structural_hint"] = json!(hint);
            }
        }
        if let Some(ref diag) = combined_lazy_diag {
            resp_value["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
            resp_value["analysis_contract"] =
                serde_json::to_value(&diag.analysis_contract).unwrap_or(json!(null));
        }

        // Store query snapshot for potential atlas_resume
        let all_complete =
            tier == atlas_engine::structs::precision::PrecisionTier::Exact && !partial;
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "trace_variable".into(),
            tool_args: args.clone(),
            lazy_window,
            created_at: Instant::now(),
            status: if all_complete {
                QueryStatus::Ready
            } else {
                QueryStatus::Partial
            },
        });
        resp_value["query_id"] = json!(query_id);

        (
            serde_json::to_string_pretty(&resp_value).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }

    pub(crate) fn handle_trace_caller_path(&mut self, args: &serde_json::Value) -> (String, bool) {
        let symbol_hex = args["symbol"].as_str().filter(|s| !s.is_empty());
        let symbol_name = args["symbol_name"].as_str().filter(|s| !s.is_empty());
        let max_depth = args["max_depth"].as_u64().unwrap_or(20) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Validate symbol_name length
        if let Some(name) = symbol_name {
            if name.len() > MAX_SYMBOL_NAME_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_callers",
                    &format!(
                        "symbol_name exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut lazy_warnings = Vec::new();
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let query_id = Self::generate_query_id();
        let investigation: Option<atlas_engine::Investigation>;

        let resp = if let Some(hex) = symbol_hex {
            let target_id: SymbolId = match hex.parse() {
                Ok(id) => id,
                Err(e) => {
                    let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                        "trace_callers",
                        &format!("Invalid symbol hex ID: {e}"),
                    );
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
            };
            // Update investigation with the target symbol
            self.update_investigation(InvestigationFocus::Symbol(target_id));
            investigation = self.investigation_state.active_investigation.clone();
            // Ensure structural data for this symbol's file
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(&target_id) {
                let outcome = self.ensure_structural_for_files(
                    [sym.file_id],
                    include_roots.clone(),
                    investigation.as_ref(),
                    Some(&query_id),
                );
                lazy_warnings = outcome.warnings;
                structural_tier = outcome.precision_tier;
                if let Some(ref lo) = outcome.lazy_outcome {
                    lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                }
            }
            let engine =
                RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
            engine.trace_callers(&target_id, max_depth)
        } else if let Some(name) = symbol_name {
            // Lazy structural: ensure name-based symbols are structurally parsed
            let outcome = self.ensure_structural_for_symbol_name(
                name,
                include_roots.clone(),
                None,
                Some(&query_id),
            );
            lazy_warnings = outcome.warnings;
            structural_tier = outcome.precision_tier;
            if let Some(ref lo) = outcome.lazy_outcome {
                lazy_diag = Some(LazyDiagnostics::from_structural(lo));
            }
            let engine =
                RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
            let result = engine.trace_callers_by_name(name, max_depth);
            // After resolution, try to find the symbol and update investigation
            if let Ok(symbols) = self.store.find_symbols_by_qname(name) {
                if let Some(sym) = symbols.first() {
                    self.update_investigation(InvestigationFocus::Symbol(sym.id));
                }
            }
            let _investigation = self.investigation_state.active_investigation.clone();
            result
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
        let mut resp = resp;
        let is_error = !resp.ok;

        // Inject warnings into diagnostics (not JSON "warnings" field)
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !lazy_warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            lazy_warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let mut resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));
        resp_value["structural_precision_tier"] =
            serde_json::to_value(structural_tier).unwrap_or(json!(null));
        if structural_tier != atlas_engine::structs::precision::PrecisionTier::Exact {
            if let Some(hint) = atlas_engine::precision::next_action_structural(structural_tier) {
                resp_value["structural_hint"] = json!(hint);
            }
        }
        if let Some(ref diag) = lazy_diag {
            resp_value["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
            resp_value["analysis_contract"] =
                serde_json::to_value(&diag.analysis_contract).unwrap_or(json!(null));
        }

        // Store query snapshot for potential atlas_resume
        let all_complete =
            structural_tier == atlas_engine::structs::precision::PrecisionTier::Exact;
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "trace_caller_path".into(),
            tool_args: args.clone(),
            lazy_window: None,
            created_at: Instant::now(),
            status: if all_complete {
                QueryStatus::Ready
            } else {
                QueryStatus::Partial
            },
        });
        resp_value["query_id"] = json!(query_id);

        (
            serde_json::to_string_pretty(&resp_value).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }

    pub(crate) fn handle_trace_forward(&mut self, args: &serde_json::Value) -> (String, bool) {
        let from_hex = args["from"].as_str().filter(|s| !s.is_empty());
        let to_hex = args["to"].as_str().filter(|s| !s.is_empty());
        let from_name = args["from_name"].as_str().filter(|s| !s.is_empty());
        let to_name = args["to_name"].as_str().filter(|s| !s.is_empty());
        let max_depth = args["max_depth"].as_u64().unwrap_or(10) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Validate from_name / to_name length
        if let Some(name) = from_name {
            if name.len() > MAX_SYMBOL_NAME_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_forward",
                    &format!(
                        "from_name exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }
        if let Some(name) = to_name {
            if name.len() > MAX_SYMBOL_NAME_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_forward",
                    &format!(
                        "to_name exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut lazy_warnings = Vec::new();
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let query_id = Self::generate_query_id();

        // Name-based lookup (new path — avoids requiring hex IDs)
        if let (Some(fname), Some(tname)) = (from_name, to_name) {
            // Lazy structural: ensure name-based symbols are structurally parsed
            for name in [fname, tname] {
                let outcome = self.ensure_structural_for_symbol_name(
                    name,
                    include_roots.clone(),
                    None,
                    Some(&query_id),
                );
                lazy_warnings.extend(outcome.warnings);
                structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
                if let Some(ref lo) = outcome.lazy_outcome {
                    // Capture the last extraction's diagnostics (covers both symbols).
                    lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                }
            }
            let engine =
                RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
            let mut resp = engine.trace_forward_by_name(fname, tname, max_depth);
            // After resolution, try to find the from_name symbol and update investigation
            if let Ok(symbols) = self.store.find_symbols_by_qname(fname) {
                if let Some(sym) = symbols.first() {
                    self.update_investigation(InvestigationFocus::Symbol(sym.id));
                }
            }
            let is_error = !resp.ok;
            // Inject warnings into diagnostics
            resp.diagnostics.extend(warnings_to_trace_diagnostics(
                root_warnings,
                "include_roots_warning",
            ));
            let lazy_partial = !lazy_warnings.is_empty();
            resp.diagnostics.extend(warnings_to_trace_diagnostics(
                lazy_warnings,
                "lazy_structural_warning",
            ));
            resp.partial_result = resp.partial_result || lazy_partial;

            let mut resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));
            resp_value["structural_precision_tier"] =
                serde_json::to_value(structural_tier).unwrap_or(json!(null));
            if structural_tier != atlas_engine::structs::precision::PrecisionTier::Exact {
                if let Some(hint) = atlas_engine::precision::next_action_structural(structural_tier)
                {
                    resp_value["structural_hint"] = json!(hint);
                }
            }
            if let Some(ref diag) = lazy_diag {
                resp_value["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
                resp_value["analysis_contract"] =
                    serde_json::to_value(&diag.analysis_contract).unwrap_or(json!(null));
            }

            // Store query snapshot for potential atlas_resume
            let all_complete =
                structural_tier == atlas_engine::structs::precision::PrecisionTier::Exact;
            self.store_snapshot(QuerySnapshot {
                query_id: query_id.clone(),
                tool_name: "trace_forward".into(),
                tool_args: args.clone(),
                lazy_window: None,
                created_at: Instant::now(),
                status: if all_complete {
                    QueryStatus::Ready
                } else {
                    QueryStatus::Partial
                },
            });
            resp_value["query_id"] = json!(query_id);

            return (
                serde_json::to_string_pretty(&resp_value).unwrap_or_else(|e| e.to_string()),
                is_error,
            );
        }

        // Hex ID path (existing behavior)
        let (from_id, to_id) = match (from_hex, to_hex) {
            (Some(f), Some(t)) => {
                let fid: SymbolId = match f.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                            "trace_forward",
                            &format!("Invalid 'from' symbol ID: {e}"),
                        );
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                };
                let tid: SymbolId = match t.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                            "trace_forward",
                            &format!("Invalid 'to' symbol ID: {e}"),
                        );
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                };
                (fid, tid)
            }
            _ => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_forward",
                    "Provide either (`from` + `to` hex IDs) or (`from_name` + `to_name` symbol names). Mixed hex/name mode is not supported.",
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // Update investigation with the from symbol
        self.update_investigation(InvestigationFocus::Symbol(from_id));
        let investigation = self.investigation_state.active_investigation.clone();

        // Ensure structural for endpoint files
        let mut file_set: std::collections::HashSet<atlas_engine::FileId> =
            std::collections::HashSet::new();
        for id in [&from_id, &to_id] {
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(id) {
                file_set.insert(sym.file_id);
            }
        }
        let outcome = self.ensure_structural_for_files(
            file_set,
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        lazy_warnings = outcome.warnings;
        structural_tier = outcome.precision_tier;
        if let Some(ref lo) = outcome.lazy_outcome {
            lazy_diag = Some(LazyDiagnostics::from_structural(lo));
        }

        let engine = RawTraceEngine::new_with_root(self.store.clone(), self.project_root.clone());
        let mut resp = engine.trace_forward(&from_id, &to_id, max_depth);
        let is_error = !resp.ok;
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !lazy_warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            lazy_warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let mut resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));
        resp_value["structural_precision_tier"] =
            serde_json::to_value(structural_tier).unwrap_or(json!(null));
        if structural_tier != atlas_engine::structs::precision::PrecisionTier::Exact {
            if let Some(hint) = atlas_engine::precision::next_action_structural(structural_tier) {
                resp_value["structural_hint"] = json!(hint);
            }
        }
        if let Some(ref diag) = lazy_diag {
            resp_value["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
            resp_value["analysis_contract"] =
                serde_json::to_value(&diag.analysis_contract).unwrap_or(json!(null));
        }

        // Store query snapshot for potential atlas_resume
        let all_complete =
            structural_tier == atlas_engine::structs::precision::PrecisionTier::Exact;
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "trace_forward".into(),
            tool_args: args.clone(),
            lazy_window: None,
            created_at: Instant::now(),
            status: if all_complete {
                QueryStatus::Ready
            } else {
                QueryStatus::Partial
            },
        });
        resp_value["query_id"] = json!(query_id);

        (
            serde_json::to_string_pretty(&resp_value).unwrap_or_else(|e| e.to_string()),
            is_error,
        )
    }
}
