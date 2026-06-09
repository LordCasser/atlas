//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.  Includes transparent lazy structural extraction with progress
//! notifications to prevent MCP timeout during on-demand extraction.

use atlas_engine::{InvestigationFocus, TraceQueryResponse};

use super::lazy_response::{LazyDiagnostics, LazyLayerDiagnostics, LazyResponse};
use super::{
    MAX_FILE_PATH_LENGTH, MAX_SYMBOL_NAME_LENGTH, ToolRouter, get_str_opt, get_u64,
    resolve_file_id, warnings_to_trace_diagnostics,
};
use crate::tools::symbol_selector::{SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_trace_point(
        &mut self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
    ) -> (String, bool) {
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
        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // Ensure structural before tracing
        let outcome = self.ensure_structural_for_files(
            [file_id],
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        // Capture lazy diagnostics before outcome fields are moved.
        let stats = self.get_capability_stats();
        let lazy_diag: Option<LazyDiagnostics> = outcome
            .lazy_outcome
            .as_ref()
            .map(|lo| LazyDiagnostics::from_structural_with_stats(lo, stats.as_ref()));

        ctx.send_progress(0.8, "Running trace point...");
        let mut resp = self
            .engine
            .lock()
            .unwrap()
            .trace_point(&file_id, line, column);
        ctx.send_progress(1.0, "Trace complete");

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
        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_structural_keys()
            .with_precision_tier(tier)
            .with_lazy_diag(lazy_diag)
            .with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
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
        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

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
        let mut resp = self
            .engine
            .lock()
            .unwrap()
            .trace_variable(&file_id, line, column, max_depth);

        // Build combined lazy diagnostics from the structural outcome.
        // Engine already injects dataflow-layer diagnostics into resp.diagnostics;
        // we surface the structural layer separately so the agent sees both.
        let stats = self.get_capability_stats();
        let mut combined_lazy_diag: Option<LazyDiagnostics> = structural_lo
            .as_ref()
            .map(|lo| LazyDiagnostics::from_structural_with_stats(lo, stats.as_ref()));

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
                    diag.dataflow = Some(LazyLayerDiagnostics::from_lazy_summary(summary));
                }
                None => {
                    combined_lazy_diag = Some(LazyDiagnostics::from_dataflow_summary(summary));
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
        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_structural_keys()
            .with_precision_tier(tier)
            .with_lazy_diag(combined_lazy_diag)
            .with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_caller_path(&mut self, args: &serde_json::Value) -> (String, bool) {
        let max_depth = args["max_depth"].as_u64().unwrap_or(20) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Parse symbol parameter as unified SymbolInput (string or structured selector).
        let input: SymbolInput = match serde_json::from_value(args["symbol"].clone()) {
            Ok(inp) => inp,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_callers",
                    &format!(
                        "Invalid symbol parameter: {e}. Accepts qualified name or SymbolSelector JSON."
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // Validate symbol name length
        let symbol_str = match &input {
            SymbolInput::Name(s) => s.as_str(),
            SymbolInput::Selector(sel) => &sel.qualified_name,
        };
        if symbol_str.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                &format!("symbol exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        if symbol_str.is_empty() {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                "Missing required 'symbol' parameter. Accepts qualified name or SymbolSelector JSON.",
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

        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // Unified symbol resolution — BestEffortSingle always picks one symbol.
        let resolution = match self
            .resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle)
        {
            Ok(r) => r,
            Err(e) => {
                return (format!("Symbol resolution error: {e}"), true);
            }
        };
        let (target_id, resolved_symbol) = match resolution {
            SymbolResolution::Single {
                symbol_id,
                resolved,
            } => (symbol_id, Some(resolved)),
            SymbolResolution::Ambiguous { candidates, .. } => {
                // BestEffortSingle with plain Name input: engine can't break
                // ties.  Pick the first candidate and look up its SymbolId
                // from the store so we can proceed with tracing.
                let first = &candidates[0];
                let sid = match self.store.find_symbols_by_qname(&first.qualified_name) {
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
                };
                (sid, None)
            }
            SymbolResolution::NotFound {
                qname,
                suggestions,
            } => {
                return (
                    format!(
                        "Symbol not found: {qname}. Suggestions: {:?}",
                        suggestions
                    ),
                    true,
                );
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
                let stats = self.get_capability_stats();
                lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                    lo,
                    stats.as_ref(),
                ));
            }
        }
        let resp = self
            .engine
            .lock()
            .unwrap()
            .trace_callers(&target_id, max_depth);
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
        if let Some(ref resolved) = resolved_symbol {
            if let Some(obj) = resp_value.as_object_mut() {
                obj.insert(
                    "resolved_symbol".to_string(),
                    serde_json::to_value(resolved).unwrap_or(json!(null)),
                );
            }
        }

        lr.with_structural_keys()
            .with_precision_tier(structural_tier)
            .with_lazy_diag(lazy_diag)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_forward(&mut self, args: &serde_json::Value) -> (String, bool) {
        let max_depth = args["max_depth"].as_u64().unwrap_or(10) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Parse 'from' parameter as unified SymbolInput.
        let from_input: SymbolInput = match parse_symbol_input(args, "from") {
            Ok(inp) => inp,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_forward",
                    &e,
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };
        // Parse 'to' parameter as unified SymbolInput.
        let to_input: SymbolInput = match parse_symbol_input(args, "to") {
            Ok(inp) => inp,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_forward",
                    &e,
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // Validate name lengths
        let from_name = match &from_input {
            SymbolInput::Name(s) => s.as_str(),
            SymbolInput::Selector(sel) => &sel.qualified_name,
        };
        let to_name = match &to_input {
            SymbolInput::Name(s) => s.as_str(),
            SymbolInput::Selector(sel) => &sel.qualified_name,
        };
        if from_name.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                &format!("from exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }
        if to_name.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                &format!("to exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        if from_name.is_empty() || to_name.is_empty() {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                "Must provide both 'from' and 'to' parameters. Accepts qualified names or SymbolSelector JSON.",
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let lazy_warnings;
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // -- Resolve 'from' symbol --
        let from_resolution = match self
            .resolve_symbol_input(&from_input, SymbolResolutionPolicy::BestEffortSingle)
        {
            Ok(r) => r,
            Err(e) => {
                return (format!("Symbol resolution error for 'from': {e}"), true);
            }
        };
        let (from_id, resolved_from) = match from_resolution {
            SymbolResolution::Single {
                symbol_id,
                resolved,
            } => (symbol_id, Some(resolved)),
            SymbolResolution::Ambiguous { candidates, .. } => {
                let first = &candidates[0];
                let sid = match self.store.find_symbols_by_qname(&first.qualified_name) {
                    Ok(symbols) => match symbols.first() {
                        Some(s) => s.id,
                        None => {
                            return (
                                format!(
                                    "From symbol '{}' found in candidates but not in store",
                                    first.qualified_name
                                ),
                                true,
                            );
                        }
                    },
                    Err(e) => return (format!("Lookup error: {e}"), true),
                };
                (sid, None)
            }
            SymbolResolution::NotFound { qname, .. } => {
                return (format!("Symbol not found: {qname}"), true);
            }
        };

        // -- Resolve 'to' symbol --
        let to_resolution = match self
            .resolve_symbol_input(&to_input, SymbolResolutionPolicy::BestEffortSingle)
        {
            Ok(r) => r,
            Err(e) => {
                return (format!("Symbol resolution error for 'to': {e}"), true);
            }
        };
        let (to_id, resolved_to) = match to_resolution {
            SymbolResolution::Single {
                symbol_id,
                resolved,
            } => (symbol_id, Some(resolved)),
            SymbolResolution::Ambiguous { candidates, .. } => {
                let first = &candidates[0];
                let sid = match self.store.find_symbols_by_qname(&first.qualified_name) {
                    Ok(symbols) => match symbols.first() {
                        Some(s) => s.id,
                        None => {
                            return (
                                format!(
                                    "To symbol '{}' found in candidates but not in store",
                                    first.qualified_name
                                ),
                                true,
                            );
                        }
                    },
                    Err(e) => return (format!("Lookup error: {e}"), true),
                };
                (sid, None)
            }
            SymbolResolution::NotFound { qname, .. } => {
                return (format!("Symbol not found: {qname}"), true);
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
        structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
        if let Some(ref lo) = outcome.lazy_outcome {
            let stats = self.get_capability_stats();
            lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                lo,
                stats.as_ref(),
            ));
        }

        let mut resp = self
            .engine
            .lock()
            .unwrap()
            .trace_forward(&from_id, &to_id, max_depth);
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
        if let Some(ref resolved) = resolved_from {
            if let Some(obj) = resp_value.as_object_mut() {
                obj.insert(
                    "resolved_from".to_string(),
                    serde_json::to_value(resolved).unwrap_or(json!(null)),
                );
            }
        }
        if let Some(ref resolved) = resolved_to {
            if let Some(obj) = resp_value.as_object_mut() {
                obj.insert(
                    "resolved_to".to_string(),
                    serde_json::to_value(resolved).unwrap_or(json!(null)),
                );
            }
        }

        lr.with_structural_keys()
            .with_precision_tier(structural_tier)
            .with_lazy_diag(lazy_diag)
            .with_is_error(is_error)
            .build(resp_value, self)
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use atlas_engine::FileId;
    use atlas_engine::FileInfo;
    use atlas_engine::Language;
    use atlas_engine::ParseStatus;
    use atlas_engine::Store;
    use atlas_engine::SymbolDef;
    use atlas_engine::SymbolId;
    use atlas_engine::SymbolKind;
    use atlas_engine::TextRange;

    // -- Test helpers -------------------------------------------------------

    fn test_store() -> Arc<Store> {
        let s = Store::open_in_memory().unwrap();
        s.init_schema().unwrap();
        Arc::new(s)
    }

    fn register_file(store: &Store, path: &str) -> FileId {
        let fid = FileId::generate(path);
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: path.into(),
                language: Language::TypeScript,
                content_hash: "hash1".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        fid
    }

    fn insert_symbol(
        store: &Store,
        file_id: FileId,
        simple_name: &str,
        qname: &str,
        kind: SymbolKind,
    ) -> SymbolId {
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let sid = SymbolId::generate(&file_id, "typescript", simple_name, kind.as_str(), None);
        let sym = SymbolDef {
            id: sid,
            kind,
            name: simple_name.into(),
            qualified_name: qname.into(),
            symbol_path: vec![simple_name.into()],
            file_id,
            language: Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();
        sid
    }

    // -- Handler tests -----------------------------------------------------

    fn new_router(store: Arc<Store>) -> ToolRouter {
        ToolRouter::new_empty(store, PathBuf::from("/tmp"))
    }

    #[test]
    fn trace_callers_hex_input_returns_not_found() {
        // Hex strings are no longer auto-detected — they are treated as
        // qualified names. A hex-looking string won't match any symbol.
        let store = test_store();
        let f = register_file(&store, "test.ts");
        let sid = insert_symbol(&store, f, "func", "func.func", SymbolKind::Function);
        let hex = sid.to_hex();

        let mut router = new_router(store);
        let args = serde_json::json!({"symbol": hex});
        let (resp_str, is_error) = router.handle_trace_caller_path(&args);
        assert!(
            resp_str.contains("not found") || is_error,
            "hex string should not resolve as SymbolId: {resp_str}"
        );
    }

    #[test]
    fn trace_variable_missing_file_returns_error() {
        let store = test_store();
        let mut router = new_router(store);

        let args = serde_json::json!({
            "file_path": "nonexistent.ts",
            "line": 1,
            "column": 1,
        });
        let (_resp_str, is_error) = router.handle_trace_variable(&args);
        assert!(is_error, "should be error for missing file");
    }

    #[test]
    fn trace_variable_missing_position_returns_error() {
        let store = test_store();
        let _f = register_file(&store, "test.ts");
        let mut router = new_router(store);

        let args = serde_json::json!({
            "file_path": "test.ts",
        });
        let (_resp_str, is_error) = router.handle_trace_variable(&args);
        assert!(is_error, "should be error without line/column");
    }

    #[test]
    fn trace_callers_empty_symbol_returns_error() {
        let store = test_store();
        let mut router = new_router(store);

        let args = serde_json::json!({"symbol": ""});
        let (_resp_str, is_error) = router.handle_trace_caller_path(&args);
        assert!(is_error, "empty symbol should error");
    }

    #[test]
    fn trace_forward_empty_params_returns_error() {
        let store = test_store();
        let mut router = new_router(store);

        let args = serde_json::json!({"from": "a", "to": ""});
        let (_resp_str, is_error) = router.handle_trace_forward(&args);
        assert!(is_error, "empty 'to' should error");
    }
}
