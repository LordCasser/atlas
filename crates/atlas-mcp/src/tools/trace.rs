//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.  Includes transparent lazy structural extraction with progress
//! notifications to prevent MCP timeout during on-demand extraction.

use atlas_engine::{InvestigationFocus, TraceDiagnostic, TraceQueryResponse};

use super::analysis_envelope::AnalysisEnvelope;
use super::{
    MAX_FILE_PATH_LENGTH, ToolRouter, get_str_opt, get_u64, resolve_file_id,
    warnings_to_trace_diagnostics,
};
use crate::tools::symbol_selector::{
    SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_trace_point(
        &self,
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
        let (_, root_warnings) = self.include_roots_from_args(args);

        let file_id = {
            let active = self.project();
            match resolve_file_id(&active.store, &active.root, file_hex, file_path) {
                Ok(Some(fid)) => fid,
                Ok(None) => {
                    let msg = if file_hex.is_some() || file_path.is_some() {
                        if active.store.count_files().unwrap_or(0) == 0 {
                            "No project facts have been materialized yet. Provide a project-relative file_path so focus can extract the local file, or run CLI `atlas index` outside MCP to prebuild a project-wide cache."
                        } else {
                            "File not found in the active project facts. Check that the file_id or file_path is correct and belongs to the opened project."
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
                    err_msg.push_str(active.store_query_runtime.not_indexed_guidance());
                    let resp: TraceQueryResponse<()> =
                        TraceQueryResponse::err("trace_point", &err_msg);
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
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
        let mut lr = AnalysisEnvelope::new("trace", args);

        // Ensure structural before tracing
        let (focus_result, focus_warnings) =
            self.prepare_focus_query(Some(atlas_engine::QueryIntent::TracePoint {
                file_id,
                line,
                column,
            }));
        if let Some(ref result) = focus_result {
            lr = super::apply_focus_result_to_lr(lr, result);
        }
        ctx.send_progress(0.8, "Running trace point...");
        let mut resp = self.project()
            .engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .trace_point(&file_id, line, column);
        ctx.send_progress(1.0, "Trace complete");

        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !focus_warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            focus_warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let is_error = !resp.ok;

        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_variable(&self, args: &serde_json::Value) -> (String, bool) {
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
        let (_, root_warnings) = self.include_roots_from_args(args);

        let file_id = {
            let active = self.project();
            match resolve_file_id(&active.store, &active.root, file_hex, file_path) {
                Ok(Some(fid)) => fid,
                Ok(None) => {
                    let msg = if file_hex.is_some() || file_path.is_some() {
                        if active.store.count_files().unwrap_or(0) == 0 {
                            "No project facts have been materialized yet. Provide a project-relative file_path so focus can extract the local file, or run CLI `atlas index` outside MCP to prebuild a project-wide cache."
                        } else {
                            "File not found in the active project facts. Check that the file_id or file_path is correct and belongs to the opened project."
                        }
                    } else {
                        "Missing file_id or file_path"
                    };
                    let resp: TraceQueryResponse<()> =
                        TraceQueryResponse::err("trace_variable", msg);
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
                Err(e) => {
                    let mut err_msg = format!("Error resolving file: {e}");
                    err_msg.push_str(active.store_query_runtime.not_indexed_guidance());
                    let resp: TraceQueryResponse<()> =
                        TraceQueryResponse::err("trace_variable", &err_msg);
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
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
        let mut lr = AnalysisEnvelope::new("trace", args);

        // Ensure structural before tracing
        let (focus_result, focus_warnings) =
            self.prepare_focus_query(Some(atlas_engine::QueryIntent::TraceVariable {
                file_id,
                line,
                column,
            }));
        if let Some(ref result) = focus_result {
            lr = crate::tools::apply_focus_result_to_lr(lr, result);
        }
        // Engine::trace_variable handles lazy dataflow orchestration + trace
        // in a single call.  The response already carries lazy_summary,
        // diagnostics, and partial_result from the dataflow layer.
        let mut resp = self.project()
            .engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .trace_variable(&file_id, line, column, max_depth);

        // Merge structural-partial flag into Engine's dataflow partial_result.
        let lazy_partial = !focus_warnings.is_empty();
        resp.partial_result = resp.partial_result || lazy_partial;
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            focus_warnings,
            "lazy_structural_warning",
        ));
        let is_error = !resp.ok;

        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_caller_path(&self, args: &serde_json::Value) -> (String, bool) {
        let max_depth = args["max_depth"].as_u64().unwrap_or(20) as usize;
        let (_, root_warnings) = self.include_roots_from_args(args);

        // Parse symbol parameter as unified SymbolInput (string or structured selector).
        let input: SymbolInput = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_callers", &e);
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
        if let Err(e) = super::validate_symbol_name_length(symbol_str) {
            return (e, true);
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

        let mut lazy_warnings = Vec::new();

        let mut lr = AnalysisEnvelope::new("trace", args);

        // Unified symbol resolution — BestEffortSingle always picks one symbol.
        let resolution = match self.resolve_graph_symbol_with_focus_retry(
            &input,
            SymbolResolutionPolicy::BestEffortSingle,
            Some("incoming".to_string()),
            Some(max_depth),
        ) {
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
                let sid = match self.project()
                    .store
                    .find_symbols_by_qname(&first.qualified_name)
                {
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
            SymbolResolution::NotFound { qname, suggestions } => {
                return self.retryable_symbol_not_found_response(
                    "trace",
                    args,
                    &qname,
                    suggestions,
                    Some(
                        "trace(kind=callers) requires the target symbol to be materialized first"
                            .into(),
                    ),
                );
            }
        };

        // Update investigation with the target symbol
        self.update_investigation(InvestigationFocus::Symbol(target_id));
        // Ensure structural data for this symbol's file
        if let Ok(Some(sym)) = self.project().store.find_symbol_by_id(&target_id) {
            let (focus_result, focus_warnings) =
                self.prepare_focus_query(Some(atlas_engine::QueryIntent::Calls {
                    symbol_name: sym.name.clone(),
                    file_id: Some(sym.file_id),
                    symbol_id: None,
                    direction: None,
                    depth: None,
                }));
            if let Some(ref result) = focus_result {
                lr = crate::tools::apply_focus_result_to_lr(lr, result);
            }
            lazy_warnings = focus_warnings;
        }
        let resp = self.project()
            .engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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

        lr.with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_forward(&self, args: &serde_json::Value) -> (String, bool) {
        let max_depth = args["max_depth"].as_u64().unwrap_or(10) as usize;
        let (_, root_warnings) = self.include_roots_from_args(args);

        // Parse 'from' parameter as unified SymbolInput.
        let from_input: SymbolInput = match parse_symbol_input(args, "from") {
            Ok(inp) => inp,
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_forward", &e);
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
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_forward", &e);
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
        if let Err(e) = super::validate_symbol_name_length(from_name) {
            return (e, true);
        }
        if let Err(e) = super::validate_symbol_name_length(to_name) {
            return (e, true);
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

        let mut lr = AnalysisEnvelope::new("trace", args);

        // -- Resolve 'from' symbol --
        let from_resolution = match self.resolve_graph_symbol_with_focus_retry(
            &from_input,
            SymbolResolutionPolicy::BestEffortSingle,
            Some("outgoing".to_string()),
            Some(max_depth),
        ) {
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
                let sid = match self.project()
                    .store
                    .find_symbols_by_qname(&first.qualified_name)
                {
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
                return self.retryable_symbol_not_found_response(
                    "trace",
                    args,
                    &qname,
                    Vec::new(),
                    Some(
                        "trace(kind=forward) requires the source symbol to be materialized first"
                            .into(),
                    ),
                );
            }
        };

        // -- Resolve 'to' symbol --
        let to_resolution = match self.resolve_graph_symbol_with_focus_retry(
            &to_input,
            SymbolResolutionPolicy::BestEffortSingle,
            None,
            Some(max_depth),
        ) {
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
                let sid = match self.project()
                    .store
                    .find_symbols_by_qname(&first.qualified_name)
                {
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
                if let Some(hint) = self.unresolved_call_target_hint(&[from_id], &qname) {
                    return self.retryable_symbol_not_found_response(
                        "trace",
                        args,
                        &qname,
                        Vec::new(),
                        Some(hint),
                    );
                }
                return self.retryable_symbol_not_found_response(
                    "trace",
                    args,
                    &qname,
                    Vec::new(),
                    Some(
                        "trace(kind=forward) requires the target symbol to be materialized first"
                            .into(),
                    ),
                );
            }
        };

        // Update investigation with the from symbol
        self.update_investigation(InvestigationFocus::Symbol(from_id));

        // Ensure structural for endpoint files via focus query
        let intent = self.project()
            .store
            .find_symbol_by_id(&from_id)
            .ok()
            .flatten()
            .map(|sym| atlas_engine::QueryIntent::Calls {
                symbol_name: sym.name.clone(),
                file_id: Some(sym.file_id),
                symbol_id: None,
                direction: None,
                depth: None,
            });
        let (focus_result, lazy_warnings) = self.prepare_focus_query(intent);
        if let Some(ref result) = focus_result {
            lr = crate::tools::apply_focus_result_to_lr(lr, result);
        }
        let has_full_index = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };

        let mut resp = self.project()
            .engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
        let has_no_path = resp
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("no_path_found"));
        if has_no_path && !has_full_index {
            resp.partial_result = true;
            resp.diagnostics.push(
                TraceDiagnostic::warning(
                    "No forward path was found in the current focus closure; this does not prove that no repo-wide path exists until full indexing or further refinement completes.",
                )
                .with_code("focus_no_path_partial"),
            );
        }

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

        lr.with_partial_result(resp.partial_result)
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

    fn insert_unresolved_call_reference(store: &Store, source: SymbolId, name: &str) {
        let source_symbol = store
            .find_symbol_by_id(&source)
            .unwrap()
            .expect("source symbol should exist");
        let range = TextRange {
            start_byte: 32,
            end_byte: 32 + name.len() as u32,
            start_line: 4,
            start_column: 8,
            end_line: 4,
            end_column: 8 + name.len() as u32,
        };
        let reference = atlas_engine::ReferenceUse {
            id: atlas_engine::ReferenceId::generate(
                &source_symbol.file_id,
                Some(&source),
                range.start_byte,
                range.end_byte,
                name,
                atlas_engine::ReferenceKind::Call,
            ),
            file_id: source_symbol.file_id,
            source_symbol: Some(source),
            scope_id: None,
            kind: atlas_engine::ReferenceKind::Call,
            text: name.to_string(),
            name: name.to_string(),
            receiver: None,
            arity: Some(1),
            range,
            binding_id: None,
            resolved: None,
        };
        store.insert_references(&[reference]).unwrap();
    }

    // -- Handler tests -----------------------------------------------------

    fn new_router(store: Arc<Store>) -> ToolRouter {
        ToolRouter::new_empty(store, PathBuf::from("/tmp"))
    }

    #[test]
    fn trace_callers_hex_input_returns_not_found() {
        // Hex strings are no longer auto-detected — they are treated as
        // qualified names. A hex-looking string won't match any symbol.
        // In focus mode (no full index), this returns a partial "building"
        // result instead of a hard error — the symbol may exist but hasn't
        // been materialized in the local closure yet.
        let store = test_store();
        let f = register_file(&store, "test.ts");
        let sid = insert_symbol(&store, f, "func", "func.func", SymbolKind::Function);
        let hex = sid.to_hex();

        let mut router = new_router(store);
        let args = serde_json::json!({"symbol": hex});
        let (resp_str, is_error) = router.handle_trace_caller_path(&args);
        assert!(
            resp_str.contains("not found")
                || resp_str.contains("not available")
                || resp_str.contains("building")
                || is_error,
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

    #[test]
    fn trace_forward_not_found_target_reports_unresolved_call_hint() {
        let store = test_store();
        let file = register_file(&store, "test.ts");
        let from_id = insert_symbol(&store, file, "sender", "sender", SymbolKind::Function);
        insert_unresolved_call_reference(&store, from_id, "copy_from_user");

        let mut router = new_router(store);
        let args = serde_json::json!({"from": "sender", "to": "copy_from_user"});
        let (resp, is_error) = router.handle_trace_forward(&args);

        // In focus mode, the response is a partial "building" result (is_error=false)
        // instead of a hard error. The symbol may exist but hasn't been materialized
        // in the local closure yet. Both error and partial-result are acceptable.
        if is_error {
            assert!(
                resp.contains("unresolved call token") && resp.contains("trace(kind=\"point\")"),
                "missing actionable unresolved-call hint: {resp}"
            );
        } else {
            assert!(
                resp.contains("building")
                    || resp.contains("not available")
                    || resp.contains("partial"),
                "focus-mode partial result should indicate building state: {resp}"
            );
        }
    }
}
