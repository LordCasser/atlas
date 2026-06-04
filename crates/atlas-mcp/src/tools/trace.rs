//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.  Includes transparent lazy structural extraction with progress
//! notifications to prevent MCP timeout during on-demand extraction.

use std::time::Instant;

use atlas_engine::SymbolId;
use atlas_engine::{InvestigationFocus, TraceQueryResponse};

use super::lazy_response::{LazyDiagnostics, LazyLayerDiagnostics};
use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{
    MAX_FILE_PATH_LENGTH, MAX_SYMBOL_NAME_LENGTH, ToolRouter, get_str, get_str_opt, get_u64,
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

        self.send_progress(0.8, "Running trace point...");
        let mut resp = self.engine.trace_point(&file_id, line, column);
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
            tool_name: "trace".into(),
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

        // Engine::trace_variable handles lazy dataflow orchestration + trace
        // in a single call.  The response already carries lazy_summary,
        // diagnostics, and partial_result from the dataflow layer.
        let mut resp = self.engine.trace_variable(&file_id, line, column, max_depth);

        // Build combined lazy diagnostics from the structural outcome.
        // Engine already injects dataflow-layer diagnostics into resp.diagnostics;
        // we surface the structural layer separately so the agent sees both.
        let mut combined_lazy_diag: Option<LazyDiagnostics> = structural_lo
            .as_ref()
            .map(|lo| LazyDiagnostics::from_structural(lo));

        // Populate dataflow diagnostics from Engine's LazySummary (P2#14).
        // After routing through Engine, the MCP layer no longer has a direct
        // LazyWindow from dataflow extraction — but Engine's response carries
        // the summary.  Convert it so the agent sees both structural AND
        // dataflow layer stats in the lazy_diagnostics block.
        //
        // When structural_lo is None (already cached), the dataflow summary
        // must still be surfaced on its own.  When structural_lo is Some, the
        // dataflow layer is populated alongside the existing structural layer.
        // This check runs independently of resp.result — dataflow extraction
        // may succeed even when no trace path is found.
        if let Some(ref summary) = resp.lazy_summary {
            match combined_lazy_diag {
                Some(ref mut diag) => {
                    diag.dataflow =
                        Some(LazyLayerDiagnostics::from_lazy_summary(summary));
                }
                None => {
                    combined_lazy_diag =
                        Some(LazyDiagnostics::from_dataflow_summary(summary));
                }
            }
        }

        // Merge structural-partial flag into Engine's dataflow partial_result.
        let lazy_partial = !outcome.warnings.is_empty();
        resp.partial_result = resp.partial_result || lazy_partial;
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            outcome.warnings,
            "lazy_structural_warning",
        ));
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
            tier == atlas_engine::structs::precision::PrecisionTier::Exact && !resp.partial_result;
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "trace".into(),
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

    pub(crate) fn handle_trace_caller_path(&mut self, args: &serde_json::Value) -> (String, bool) {
        let symbol = get_str(args, "symbol");
        let max_depth = args["max_depth"].as_u64().unwrap_or(20) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Validate symbol length
        if symbol.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                &format!("symbol exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        if symbol.is_empty() {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                "Missing required 'symbol' parameter. Accepts qualified name or hex SymbolId. Auto-detects format.",
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut lazy_warnings = Vec::new();
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let query_id = Self::generate_query_id();

        // Auto-detect symbol: try hex parse first, then qname resolution.
        let is_hex = symbol.len() >= 8 && symbol.chars().all(|c| c.is_ascii_hexdigit());
        let target_id: SymbolId = if is_hex {
            match symbol.parse() {
                Ok(id) => id,
                Err(_) => match self.store.find_symbols_by_qname(symbol) {
                    Ok(refs) if !refs.is_empty() => refs[0].id,
                    _ => {
                        let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                            "trace_callers",
                            &format!(
                                "Symbol not found by qname: '{symbol}'. Tip: the 'symbol' parameter accepts both hex SymbolIds and qualified names. Auto-detects format."
                            ),
                        );
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                },
            }
        } else {
            match self.store.find_symbols_by_qname(symbol) {
                Ok(refs) if !refs.is_empty() => refs[0].id,
                _ => {
                    // Lazy structural fallback: try name-based lookup with structural extraction
                    let outcome = self.ensure_structural_for_symbol_name(
                        symbol,
                        include_roots.clone(),
                        None,
                        Some(&query_id),
                    );
                    lazy_warnings.extend(outcome.warnings);
                    structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
                    if let Some(ref lo) = outcome.lazy_outcome {
                        lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                    }
                    // Re-query after lazy extraction
                    match self.store.find_symbols_by_qname(symbol) {
                        Ok(refs) if !refs.is_empty() => refs[0].id,
                        _ => {
                            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                                "trace_callers",
                                &format!(
                                    "Symbol not found: '{symbol}'. Tip: the 'symbol' parameter accepts both hex SymbolIds and qualified names. Try 'search' first to discover the correct qualified name."
                                ),
                            );
                            return (
                                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                                true,
                            );
                        }
                    }
                }
            }
        };

        // Update investigation with the target symbol
        self.update_investigation(InvestigationFocus::Symbol(target_id));
        let investigation = self.investigation_state.active_investigation.clone();
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
        let resp = self.engine.trace_callers(&target_id, max_depth);
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
            tool_name: "trace".into(),
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
        let from = get_str(args, "from");
        let to = get_str(args, "to");
        let max_depth = args["max_depth"].as_u64().unwrap_or(10) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Validate from / to length
        if from.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                &format!("from exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }
        if to.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                &format!("to exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        if from.is_empty() || to.is_empty() {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                "Must provide both 'from' and 'to' parameters. Accepts qualified names or hex SymbolIds. Auto-detects format.",
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut lazy_warnings = Vec::new();
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let query_id = Self::generate_query_id();

        // Auto-detect from/to: try hex parse first, then qname resolution.
        let mut resolve_to_id = |input: &str, field: &str| -> Result<SymbolId, String> {
            let is_hex = input.len() >= 8 && input.chars().all(|c| c.is_ascii_hexdigit());
            if is_hex {
                if let Ok(id) = input.parse::<SymbolId>() {
                    return Ok(id);
                }
                // Fall through to qname resolution
            }
            match self.store.find_symbols_by_qname(input) {
                Ok(refs) if !refs.is_empty() => Ok(refs[0].id),
                _ => {
                    // Lazy structural fallback: try name-based lookup with structural extraction
                    let outcome = self.ensure_structural_for_symbol_name(
                        input,
                        include_roots.clone(),
                        None,
                        Some(&query_id),
                    );
                    lazy_warnings.extend(outcome.warnings);
                    if outcome.precision_tier < structural_tier {
                        structural_tier = outcome.precision_tier;
                    }
                    if let Some(ref lo) = outcome.lazy_outcome {
                        lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                    }
                    // Re-query after lazy extraction
                    match self.store.find_symbols_by_qname(input) {
                        Ok(refs) if !refs.is_empty() => Ok(refs[0].id),
                        _ => Err(format!(
                            "Symbol not found: '{input}'. The '{field}' parameter accepts both hex SymbolIds and qualified names. Try 'search' first to discover the correct qualified name."
                        )),
                    }
                }
            }
        };

        let from_id: SymbolId = match resolve_to_id(from, "from") {
            Ok(id) => id,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_forward", &e);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };
        let to_id: SymbolId = match resolve_to_id(to, "to") {
            Ok(id) => id,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_forward", &e);
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

        let mut resp = self.engine.trace_forward(&from_id, &to_id, max_depth);
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
            tool_name: "trace".into(),
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
