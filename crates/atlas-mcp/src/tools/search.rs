//! Search tools: scoped symbol search and single-symbol lookup.
//!
//! Search delegates to [`atlas_engine::ScopedSearchService`] for the 3-level
//! FTS → exact → LIKE search with Auto-mode lazy triggering.  The MCP layer
//! only translates JSON args into a [`ScopedSearchRequest`] and converts the
//! engine response back to the MCP JSON format.

use atlas_engine::Engine;
use atlas_engine::FileId;
use atlas_engine::InvestigationFocus;
use atlas_engine::ScopedSearchRequest;
use atlas_engine::ScopedSearchService;
use atlas_engine::SearchAnalysis;
use atlas_engine::SearchCoverage;
use atlas_engine::SearchResult;
use atlas_engine::SymbolKind;

use super::analysis_envelope::{AnalysisEnvelope, GapRecord};
use super::{MAX_QUERY_LENGTH, ToolRouter, add_json_warnings, get_str, get_str_opt, get_u64};
use crate::tools::symbol_selector::{
    ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};

use serde_json::json;
use std::sync::Arc;

// ── MCP response helpers ────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
struct SearchHit {
    name: String,
    qualified_name: String,
    kind: String,
    language: String,
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

        // Construct a fresh Engine from the shared Store.  ScopedSearchService
        // needs an Arc<Engine> but the router holds a Mutex<Engine>.
        // Engine::from_store is a lightweight constructor — all inner services
        // share the same Arc<Store>.
        let engine: Arc<Engine> = Arc::new(Engine::from_store(
            self.project().store.clone(),
            Some(&self.project().root),
        ));
        let svc = ScopedSearchService::new_with_project_root(
            self.project().store.clone(),
            engine,
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
                .with_analysis_summary(if search_has_hits {
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
