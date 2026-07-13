//! Search tools: scoped symbol search and single-symbol lookup.
//!
//! Search delegates to [`atlas_engine::ScopedSearchService`] for the 3-level
//! FTS → exact → LIKE search with Auto-mode lazy triggering.  The MCP layer
//! only translates JSON args into a [`ScopedSearchRequest`] and converts the
//! engine response back to the MCP JSON format.

use atlas_engine::FileId;
use atlas_engine::InvestigationFocus;
use atlas_engine::ScopedSearchRequest;
use atlas_engine::ScopedSearchService;
use atlas_engine::SearchAnalysis;
use atlas_engine::SearchCoverage;
use atlas_engine::SearchResult;
use atlas_engine::SymbolKind;

use super::analysis_envelope::{AnalysisEnvelope, GapRecord};
use super::{
    MAX_QUERY_LENGTH, ToolCallContext, ToolRouter, add_json_warnings, get_str, get_str_opt,
    get_u64, is_definition_kind, normalize_project_relative_path,
};
use crate::tools::symbol_selector::{
    ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};

use serde_json::{Value, json};

// ── MCP response helpers ────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
struct SearchHit {
    name: String,
    qualified_name: String,
    kind: String,
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    score: f64,
    file: String,
    line: u32,
    layer: String,
}

impl ToolRouter {
    // ── handle_search ────────────────────────────────────────────────────

    pub(crate) fn handle_search(
        &self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let query = get_str(args, "query");
        if query.len() > MAX_QUERY_LENGTH {
            return (
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "error": format!(
                        "query exceeds maximum length of {} characters",
                        MAX_QUERY_LENGTH
                    ),
                }))
                .unwrap_or_else(|e| e.to_string()),
                true,
            );
        }
        let limit = (get_u64(args, "limit").unwrap_or(20) as usize).min(200);
        let kind = get_str_opt(args, "kind");
        let scope = get_str_opt(args, "scope")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // When a manual full structural index exists (built via CLI `atlas index`),
        // the search analysis mode is set to Manifest (structural facts are already
        // in the store) instead of Auto (lazy triggering).  Scope is still required
        // — it defines the search boundary.
        let is_manual_full = self
            .project()
            .query_runtime
            .has_full_index(&self.project().store);

        let scope = match scope {
            Some(s) => s.to_string(),
            None => {
                return (
                    serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "error": "search requires a non-empty project-relative scope such as \"src\", \"kernel/sched\", or \"drivers/net\"; without scope, search does not perform extraction or follow-up parsing",
                        "query": query,
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        self.handle_search_sync(
            ctx,
            args,
            query,
            limit,
            kind,
            &scope,
            is_manual_full,
            include_roots,
            root_warnings,
        )
    }

    // ── handle_search_sync ────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn handle_search_sync(
        &self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: &str,
        is_manual_full: bool,
        include_roots: Vec<atlas_engine::IncludeRoot>,
        root_warnings: Vec<String>,
    ) -> (String, bool) {
        ctx.send_progress(0.1, &format!("Searching for '{query}' in {scope}..."));

        // Build the search request → delegate to ScopedSearchService.
        let kind_filter = kind.and_then(SymbolKind::from_str);
        let analysis = if is_manual_full {
            SearchAnalysis::Manifest
        } else {
            SearchAnalysis::Auto
        };
        let include_roots_strs: Vec<String> =
            include_roots.iter().map(|r| r.path.clone()).collect();

        let req = ScopedSearchRequest {
            query: query.to_string(),
            scope: Some(scope.to_string()),
            kind: kind_filter,
            analysis,
            limit,
            include_roots: include_roots_strs,
            ..Default::default()
        };

        // Reuse project Focus materialize structural stack (no throwaway Engine).
        let structural = self.project().materialize.structural().clone();
        let svc = ScopedSearchService::new_with_project_root(
            self.project().store.clone(),
            structural,
            Some(self.project().root.clone()),
        );

        let engine_resp = match svc.execute(req) {
            Ok(r) => r,
            Err(err) => {
                let mut s = format!("Search error: {err}");
                s.push_str(self.project().store_query_runtime.not_indexed_guidance());
                return (s, true);
            }
        };

        // Unavailable means no data at all — not even manifest extraction.
        // Convert to a clear error instead of returning empty results.
        if engine_resp.quality.is_unavailable() && engine_resp.scope_file_count == 0 {
            let guidance = self.project().store_query_runtime.not_indexed_guidance();
            let mut msg =
                format!("scope \"{scope}\" has no indexed data and cannot return any results");
            if !guidance.is_empty() {
                msg.push_str(&format!("\n\n{guidance}"));
            }
            let error_body = json!({
                "ok": false,
                "error": msg,
                "query": query,
                "scope": scope,
            });
            let lr = AnalysisEnvelope::new("search", args).with_is_error(true);
            return lr.build(error_body, self);
        }

        let search_is_partial = matches!(engine_resp.coverage, SearchCoverage::Partial { .. });
        let background_focus = if engine_resp.deferred_file_ids.is_empty() {
            None
        } else {
            self.enqueue_background_file_focus(&engine_resp.deferred_file_ids)
        };
        let (background_pending, retry_after_ms) = background_focus
            .as_ref()
            .map(|result| result.pending_work_count_and_eta_ms())
            .unwrap_or((0, 0));
        let background_search_pending = background_pending > 0;

        // Build the MCP JSON response from the engine response.
        let hits: Vec<SearchHit> = engine_resp
            .results
            .iter()
            .map(Self::search_result_to_hit)
            .collect();

        let mut response = json!({
            "query": query,
            "scope": scope,
            "results": hits,
            "total": engine_resp.total,
            "scope_file_count": engine_resp.scope_file_count,
        });

        response["coverage"] = match &engine_resp.coverage {
            SearchCoverage::Full => json!({"state": "complete"}),
            SearchCoverage::Partial { reason } => {
                json!({"state": "partial", "reason": reason})
            }
        };
        ctx.send_progress(1.0, &format!("Search complete ({} results)", hits.len()));

        let mut lr = AnalysisEnvelope::new("search", args)
            .with_root_warnings(root_warnings)
            .with_lazy_warnings(engine_resp.warnings)
            .with_is_error(false);
        if search_is_partial {
            let search_has_hits = !hits.is_empty();
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_summary(if background_search_pending {
                    "scoped search returned a bounded result; background Focus warming is preparing more candidate files"
                        .into()
                } else if search_has_hits {
                    "scoped search returned matches from the current focus scope; full repository coverage is unavailable"
                        .into()
                } else {
                    "scoped search returned no matches in the bounded pass; full repository coverage is unavailable"
                        .into()
                })
                .with_analysis_basis(vec!["manifest".into(), "structural".into()])
                .with_gap_records(vec![GapRecord {
                    scope: scope.to_string(),
                    reason: "closure_boundary".into(),
                    detail: "Search covered the current scoped facts, but structural coverage is incomplete for part of the scope."
                        .into(),
                }]);
            if background_search_pending {
                lr = lr.with_analysis_retry_after_ms(retry_after_ms);
            }
        }
        if let Some(result) = background_focus {
            lr = lr.with_focus_result(result);
        }
        if !search_is_partial {
            lr = lr
                .with_analysis_scope("local".into())
                .with_analysis_summary("scoped search coverage is complete".into());
        }
        lr.build(response, self)
    }

    // ── symbol detail ─────────────────────────────────────────────────────

    /// Check if the selector's file_path matches any file in the store.
    /// Returns a diagnostic string if a file_path was provided but not found.
    fn file_path_diagnostic(&self, input: &SymbolInput) -> Option<String> {
        match input {
            SymbolInput::Selector(sel) => {
                if let Some(ref fp) = sel.file_path {
                    let replaced = fp.trim_start_matches("./").replace('\\', "/");
                    let normalized = replaced.trim_end_matches('/');
                    if normalized.is_empty() {
                        return None;
                    }
                    let file_id = FileId::generate(normalized);
                    if self
                        .project()
                        .store
                        .get_file(&file_id)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        return Some(format!(
                            "file_path '{fp}' does not match any file in the project"
                        ));
                    }
                }
                None
            }
            SymbolInput::Name(_) => None,
        }
    }

    pub(crate) fn handle_symbol_detail(&self, args: &serde_json::Value) -> (String, bool) {
        // Accept both "symbol" (structured selector from handle_symbol)
        // and "qualified_name" (legacy from handle_symbol_by_position / resume_query).
        let (qname, symbol_input) = if let Ok(input) = parse_symbol_input(args, "symbol") {
            let name = match &input {
                SymbolInput::Name(s) => s.clone(),
                SymbolInput::Selector(sel) => sel.qualified_name.clone(),
            };
            (name, input)
        } else {
            let name = get_str(args, "qualified_name").to_string();
            (name.clone(), SymbolInput::Name(name))
        };
        if let Err(e) = super::validate_symbol_name_length(&qname) {
            return (e, true);
        }
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        let mut lr = AnalysisEnvelope::new("symbol", args);
        let resolution =
            self.resolve_symbol_input(&symbol_input, SymbolResolutionPolicy::UniqueOrCandidates);
        let sym;
        let lazy_warnings;
        match resolution {
            Ok(SymbolResolution::Single { symbol_id, .. }) => {
                // Found a unique symbol on the first try
                if let Ok(Some(s)) = self.project().store.find_symbol_by_id(&symbol_id) {
                    self.update_investigation(InvestigationFocus::Symbol(s.id));
                    let source_missing = include_code
                        && self
                            .project()
                            .store_query_runtime
                            .read_symbol_source(&s.id)
                            .is_none();
                    if source_missing {
                        let (focus_result, focus_warnings) = self.prepare_focus_query_with_roots(
                            Some(atlas_engine::QueryIntent::Context {
                                symbol_name: qname.to_string(),
                                file_id: Some(s.file_id),
                                symbol_id: None,
                            }),
                            include_roots.clone(),
                        );
                        if let Some(ref result) = focus_result {
                            lr = crate::tools::apply_focus_result_to_lr(lr, result);
                        }
                        lazy_warnings = focus_warnings;
                        // Structural replacement may update metadata or ranges.
                        sym = match self.resolve_symbol_input(
                            &symbol_input,
                            SymbolResolutionPolicy::UniqueOrCandidates,
                        ) {
                            Ok(SymbolResolution::Single {
                                symbol_id: new_id, ..
                            }) => self
                                .project()
                                .store
                                .find_symbol_by_id(&new_id)
                                .unwrap_or_default()
                                .unwrap_or(s),
                            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                                let diag = self.file_path_diagnostic(&symbol_input);
                                let amb_resp = Self::build_ambiguous_symbol_body(
                                    &qname,
                                    &candidates,
                                    diag.as_deref(),
                                );
                                return lr
                                    .with_is_error(true)
                                    .with_lazy_warnings(lazy_warnings)
                                    .build_with_args(amb_resp, args, self);
                            }
                            _ => s,
                        };
                    } else {
                        lazy_warnings = Vec::new();
                        sym = s;
                    }
                } else {
                    let mut err = format!("Symbol not found: {qname}");
                    err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                    return (err, true);
                }
            }
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                let diag = self.file_path_diagnostic(&symbol_input);
                let amb_resp =
                    Self::build_ambiguous_symbol_body(&qname, &candidates, diag.as_deref());
                return lr.with_is_error(true).build_with_args(amb_resp, args, self);
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                // Not found in manifest — trigger lazy structural extraction
                let has_file_hint = matches!(
                    &symbol_input,
                    SymbolInput::Selector(sel)
                        if sel.file_path.as_deref().is_some_and(|p| !p.trim().is_empty())
                );
                let mut selector_file_id = self.resolve_selector_file_id(&symbol_input);
                if selector_file_id.is_none() && has_file_hint {
                    let _ = self.prepare_focus_query_with_roots(
                        Some(atlas_engine::QueryIntent::Context {
                            symbol_name: qname.to_string(),
                            file_id: None,
                            symbol_id: None,
                        }),
                        include_roots.clone(),
                    );
                    selector_file_id = self.resolve_selector_file_id(&symbol_input);
                }
                let (focus_result, focus_warnings) = self.prepare_focus_query_with_roots(
                    Some(atlas_engine::QueryIntent::Context {
                        symbol_name: qname.to_string(),
                        file_id: selector_file_id,
                        symbol_id: None,
                    }),
                    include_roots.clone(),
                );
                if let Some(ref result) = focus_result {
                    lr = crate::tools::apply_focus_result_to_lr(lr, result);
                }
                lazy_warnings = focus_warnings;
                match self
                    .resolve_symbol_input(&symbol_input, SymbolResolutionPolicy::UniqueOrCandidates)
                {
                    Ok(SymbolResolution::Single { symbol_id, .. }) => {
                        if let Ok(Some(s)) = self.project().store.find_symbol_by_id(&symbol_id) {
                            self.update_investigation(InvestigationFocus::Symbol(s.id));
                            sym = s;
                        } else {
                            let mut err = format!("Symbol not found: {qname}");
                            err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                            return (err, true);
                        }
                    }
                    Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                        let diag = self.file_path_diagnostic(&symbol_input);
                        let amb_resp =
                            Self::build_ambiguous_symbol_body(&qname, &candidates, diag.as_deref());
                        return lr
                            .with_is_error(true)
                            .with_lazy_warnings(lazy_warnings)
                            .build_with_args(amb_resp, args, self);
                    }
                    Ok(SymbolResolution::NotFound { .. }) => {
                        let mut err = format!("Symbol not found: {qname}");
                        err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                        return (err, true);
                    }
                    Err(err) => {
                        return (err, true);
                    }
                }
            }
            Err(e) => return (e, true),
        };
        let mut result = json!({
            "name": sym.name, "qualified_name": sym.qualified_name,
            "kind": sym.kind.as_str(), "language": sym.language.as_str(),
            "visibility": sym.visibility.as_ref().map(|v| v.as_str()), "signature": sym.signature,
            "exported": sym.exported, "async": sym.async_,
            "file": self.project().store_query_runtime.resolve_file_path(&sym.file_id),
            "range": { "line": sym.range.start_line, "column": sym.range.start_column },
        });
        if include_code {
            if let Some(src) = self
                .project()
                .store_query_runtime
                .read_symbol_source(&sym.id)
            {
                result["source"] = json!(src);
            }
        }
        // Surface include_roots and lazy-structural warnings to the caller.
        add_json_warnings(&mut result, root_warnings, lazy_warnings);

        // Prepare stored_args for snapshot (add view="detail")
        let mut stored_args = args.clone();
        if let Some(obj) = stored_args.as_object_mut() {
            obj.insert("view".into(), serde_json::Value::String("detail".into()));
        }

        lr.with_root_warnings(Vec::new()) // already merged via add_json_warnings above
            .with_lazy_warnings(Vec::new()) // already merged via add_json_warnings above
            .with_is_error(false)
            .build_with_args(result, &stored_args, self)
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Convert an engine [`SearchResult`] into an MCP
    /// JSON-serializable [`SearchHit`].
    fn search_result_to_hit(result: &SearchResult) -> SearchHit {
        let sym = &result.symbol;
        SearchHit {
            name: sym.name.clone(),
            qualified_name: sym.qualified_name.clone(),
            kind: sym.kind.as_str().to_string(),
            language: sym.language.as_str().to_string(),
            signature: sym.signature.clone(),
            score: result.score.name_score,
            file: result.file_path.clone().unwrap_or_default(),
            line: sym.range.start_line,
            layer: sym.layer.clone(),
        }
    }

    /// Build the body for an ambiguous-symbol error response (without envelope).
    /// Callers should wrap this via AnalysisEnvelope.
    fn build_ambiguous_symbol_body(
        qname: &str,
        candidates: &[ScoredCandidate],
        diagnostic: Option<&str>,
    ) -> serde_json::Value {
        let candidates_json: Vec<serde_json::Value> = candidates
            .iter()
            .take(10)
            .map(|c| {
                json!({
                    "symbol_ref": {
                        "qualified_name": c.qualified_name,
                        "file_path": c.file_path,
                        "line": c.line,
                        "kind": c.kind,
                    }
                })
            })
            .collect();
        let hint = match diagnostic {
            Some(d) => format!(
                "Symbol '{}' is ambiguous ({} matches). {} Use the symbol_ref from a candidate below.",
                qname,
                candidates.len(),
                d
            ),
            None => format!(
                "Symbol '{}' is ambiguous ({} matches). Use the symbol_ref from a candidate below.",
                qname,
                candidates.len()
            ),
        };
        json!({
            "ok": false,
            "error": hint,
            "candidates": candidates_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use atlas_engine::Store;
    use serde_json::Value;

    use super::*;

    #[test]
    fn cold_search_tracks_one_retryable_job_per_candidate_directory_and_converges() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for idx in 0..3 {
            std::fs::write(
                src.join(format!("widget{idx}.ts")),
                "export function Widget() {}\nWidget();\n",
            )
            .unwrap();
        }

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let router = ToolRouter::new_empty(store, root.path().to_path_buf());

        let (body, is_error) = router.handle_search(
            &ToolCallContext::empty(),
            &serde_json::json!({"query": "Widget", "scope": "src", "limit": 10}),
        );

        assert!(!is_error, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["coverage"]["state"], "partial", "{value}");
        assert!(
            value["analysis"]["retry_after_ms"].as_u64().is_some(),
            "{value}"
        );
        assert!(
            value.get("pending_background_jobs").is_none(),
            "internal job counts must not leak into the public response: {value}"
        );
        assert!(value.get("gaps").is_none(), "{value}");

        let query_id = value["query_id"].as_str().unwrap().to_string();
        let snapshot = router
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap()
            .get(&query_id)
            .cloned()
            .expect("search query snapshot");
        let focus = snapshot.focus_result.expect("tracked search focus result");
        assert_eq!(
            focus.pending_closure_ids.len(),
            1,
            "all deferred candidates are in the same directory"
        );

        let mut resumed = value;
        for _ in 0..50 {
            if resumed["analysis"].get("retry_after_ms").is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            let (body, is_error) =
                router.handle_resume_query(&serde_json::json!({"query_id": query_id}));
            assert!(!is_error, "{body}");
            resumed = serde_json::from_str(&body).unwrap();
        }
        assert!(
            resumed["analysis"].get("retry_after_ms").is_none(),
            "search retry must converge: {resumed}"
        );
        assert_eq!(resumed["coverage"]["state"], "complete", "{resumed}");
        assert_eq!(resumed["total"], 3, "{resumed}");
        assert_eq!(resumed["results"].as_array().unwrap().len(), 3, "{resumed}");
    }
}

impl ToolRouter {
    /// Handle `symbol` tool - dispatch by `view` to sub-handlers.
    /// Remaps `symbol` -> `qualified_name` (detail) or passes through as `symbol` (context/usages).
    pub(crate) fn handle_symbol(&self, ctx: &ToolCallContext, args: &Value) -> (String, bool) {
        // Position-based lookup: file_path + line as alternative to 'symbol'
        let file_path = get_str(args, "file_path");
        let line_opt = args.get("line").and_then(|v| v.as_u64()).map(|v| v as u32);
        if let Some(line) = line_opt.filter(|_| !file_path.is_empty()) {
            return self.handle_symbol_by_position(ctx, file_path, line, args);
        }

        let view = get_str(args, "view");
        // Parse symbol uniformly - handles string, object, and stringified-JSON
        let input = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = match &input {
            SymbolInput::Name(s) => s.clone(),
            SymbolInput::Selector(sel) => sel.qualified_name.clone(),
        };
        if qname.is_empty() {
            return ("Missing required 'symbol' parameter".to_string(), true);
        }

        match view {
            "detail" | "" => {
                // Pass original symbol value (string or structured selector) so
                // handle_symbol_detail can apply file_path/kind/line filtering.
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    args.get("symbol")
                        .cloned()
                        .unwrap_or(Value::String(qname.clone())),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_symbol_detail(&Value::Object(mapped))
            }
            "context" => {
                // Pass original symbol value - sub-handler parses via parse_symbol_input
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    args.get("symbol")
                        .cloned()
                        .unwrap_or(Value::String(qname.clone())),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("includeFilePeers") {
                    mapped.insert("includeFilePeers".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_context(ctx, &Value::Object(mapped))
            }
            "usages" => {
                // Pass original symbol value - sub-handler parses via parse_symbol_input
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    args.get("symbol")
                        .cloned()
                        .unwrap_or(Value::String(qname.clone())),
                );
                if let Some(v) = args.get("limit") {
                    mapped.insert("limit".into(), v.clone());
                }
                self.handle_usages(&Value::Object(mapped))
            }
            other => (
                format!("Unknown view: '{other}'. Must be one of: detail, context, usages"),
                true,
            ),
        }
    }

    /// Handle symbol lookup by file position (`file_path` + `line` + optional `column`).
    ///
    /// Resolves the position to the nearest enclosing symbol definition, then
    /// delegates to [`handle_symbol_detail`] with the found `qualified_name`.
    fn handle_symbol_by_position(
        &self,
        ctx: &ToolCallContext,
        file_path: &str,
        line: u32,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let column = args
            .get("column")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(1);

        // Normalize and resolve file_path to FileId
        let normalized = match normalize_project_relative_path(file_path) {
            Some(p) => p,
            None => {
                return (
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": false,
                        "error": format!(
                            "Invalid file path: '{}'. Path must be project-relative and must not escape the project root.",
                            file_path
                        ),
                    }))
                    .unwrap_or_default(),
                    true,
                );
            }
        };
        let file_id = FileId::generate(&normalized);
        if self
            .project()
            .store
            .get_file(&file_id)
            .ok()
            .flatten()
            .is_none()
        {
            return (
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": false,
                    "error": format!(
                        "File not found in project: '{}'. Use the 'files' action on the 'project' tool to list indexed files.",
                        file_path
                    ),
                }))
                .unwrap_or_default(),
                true,
            );
        }

        // Ensure structural layer is available for this file
        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        let (_focus_result, focus_warnings) =
            self.prepare_focus_query_with_roots(None, include_roots);
        let mut warnings: Vec<String> = root_warnings;
        warnings.extend(focus_warnings);

        // Find all symbols in the file
        let symbols = match self.project().store.find_symbols_by_file(&file_id) {
            Ok(syms) => syms,
            Err(e) => {
                let mut err = serde_json::json!({
                    "ok": false,
                    "error": format!("Failed to read symbols for '{}': {}", file_path, e),
                });
                add_json_warnings(&mut err, warnings, vec![]);
                return (serde_json::to_string_pretty(&err).unwrap_or_default(), true);
            }
        };

        // Filter: symbol range must contain the position AND be a definition kind.
        // TextRange uses 0-based lines/columns; user input is 1-based.
        let target_line = line.saturating_sub(1);
        let target_col = column;
        let mut candidates: Vec<&atlas_engine::SymbolDef> = symbols
            .iter()
            .filter(|s| {
                is_definition_kind(&s.kind)
                    && s.range.start_line <= target_line
                    && target_line <= s.range.end_line
                    && s.range.start_column <= target_col
                    && target_col <= s.range.end_column
            })
            .collect();

        if candidates.is_empty() {
            let mut err = serde_json::json!({
                "ok": false,
                "error": format!(
                    "No symbol definition found at {}:{} (column {})",
                    file_path, line, column
                ),
            });
            add_json_warnings(&mut err, warnings, vec![]);
            return (serde_json::to_string_pretty(&err).unwrap_or_default(), true);
        }

        // Pick innermost (smallest range): sort by (line_span, column_span)
        candidates.sort_by_key(|s| {
            (s.range.end_line - s.range.start_line) * 1_000_000
                + (s.range.end_column - s.range.start_column)
        });
        let symbol = candidates[0];

        // Dispatch to the appropriate sub-handler based on view.
        let view = args.get("view").and_then(|v| v.as_str()).unwrap_or("");

        match view {
            "detail" | "" => {
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "qualified_name".into(),
                    serde_json::Value::String(symbol.qualified_name.clone()),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }

                let (mut result, is_error) =
                    self.handle_symbol_detail(&serde_json::Value::Object(mapped));
                if !warnings.is_empty() && !is_error {
                    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&result) {
                        add_json_warnings(&mut parsed, warnings.clone(), vec![]);
                        if let Ok(pretty) = serde_json::to_string_pretty(&parsed) {
                            result = pretty;
                        }
                    }
                }
                (result, is_error)
            }
            "context" => {
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    serde_json::Value::String(symbol.qualified_name.clone()),
                );
                if let Some(v) = args.get("includeCode") {
                    mapped.insert("includeCode".into(), v.clone());
                }
                if let Some(v) = args.get("includeFilePeers") {
                    mapped.insert("includeFilePeers".into(), v.clone());
                }
                if let Some(v) = args.get("include_roots") {
                    mapped.insert("include_roots".into(), v.clone());
                }
                self.handle_context(ctx, &serde_json::Value::Object(mapped))
            }
            "usages" => {
                let mut mapped = serde_json::Map::new();
                mapped.insert(
                    "symbol".into(),
                    serde_json::Value::String(symbol.qualified_name.clone()),
                );
                if let Some(v) = args.get("limit") {
                    mapped.insert("limit".into(), v.clone());
                }
                self.handle_usages(&serde_json::Value::Object(mapped))
            }
            other => (
                format!("Unknown view: '{other}'. Must be one of: detail, context, usages"),
                true,
            ),
        }
    }
}
