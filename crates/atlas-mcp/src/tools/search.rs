//! Search tools: scoped symbol search and single-symbol lookup.
//!
//! Search delegates to [`atlas_engine::ScopedSearchService`] for the 3-level
//! FTS → exact → LIKE search with Auto-mode lazy triggering.  The MCP layer
//! only translates JSON args into a [`ScopedSearchRequest`] and converts the
//! engine response back to the MCP JSON format.

use atlas_engine::Engine;
use atlas_engine::InvestigationFocus;
use atlas_engine::ScopedSearchRequest;
use atlas_engine::ScopedSearchService;
use atlas_engine::SearchAnalysis;
use atlas_engine::SearchCoverage;
use atlas_engine::SearchResult;
use atlas_engine::SymbolKind;

use super::lazy_response::{LazyDiagnostics, LazyResponse};
use crate::tools::symbol_selector::{SymbolInput, SymbolResolution, SymbolResolutionPolicy, ScoredCandidate};
use super::{MAX_QUERY_LENGTH, MAX_SYMBOL_NAME_LENGTH, ToolRouter, add_json_warnings, get_str, get_str_opt, get_u64};

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
        &mut self,
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
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // When a manual full structural index exists (built via CLI `atlas index`),
        // scope restrictions are lifted and lazy structural is disabled — all
        // files already have complete structural facts.
        let is_manual_full = self.cache.has_manual_full_index(&self.store);

        let scope = match scope {
            Some(s) => s.to_string(),
            None if is_manual_full => {
                // Manual full index: allow unscoped search on entire project.
                ".".to_string()
            }
            None => {
                return (
                    serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "error": "search requires a non-empty scope",
                        "query": query,
                        "hint": "Pass a project-relative directory or file path such as \"src\", \"kernel/sched\", or \"drivers/net\". Without scope, search does not perform extraction or follow-up parsing."
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        if background {
            return self.handle_search_background(
                query,
                limit,
                kind,
                &scope,
                is_manual_full,
                include_roots,
                root_warnings,
            );
        }
        let (result_str, is_err) = self.handle_search_sync(
            ctx,
            args,
            query,
            limit,
            kind,
            &scope,
            is_manual_full,
            include_roots,
            root_warnings,
        );
        // ScopedSearchService writes directly to the shared Store;
        // refresh the graph if the store signature changed.
        let _ = self.maybe_refresh_graph();
        (result_str, is_err)
    }

    // ── handle_search_sync ────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn handle_search_sync(
        &mut self,
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

        if !self.has_indexed_files() {
            return (
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "error": "No indexed files found.",
                    "query": query,
                    "scope": scope,
                    "next_action": {
                        "tool": "index",
                        "args": { "background": true },
                        "reason": "Build the fast manifest layer first. Atlas MCP stays in lazy mode; scoped search/context/trace will do deeper parsing on demand."
                    },
                    "ux": {
                        "mode": "lazy",
                        "startup_policy": "do_not_full_index_on_connect",
                        "after_index": "retry search with a project-relative scope such as drivers/net, kernel/sched, include/linux, or a specific file"
                    }
                }))
                .unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

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
            self.store.clone(),
            Some(&self.project_root),
        ));
        let svc = ScopedSearchService::new(self.store.clone(), engine);

        let engine_resp = match svc.execute(req) {
            Ok(r) => r,
            Err(err) => {
                let mut s = format!("Search error: {err}");
                s.push_str(self.index_not_run_guidance());
                return (s, true);
            }
        };

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
            "triggered_lazy": engine_resp.triggered_lazy,
            "analysis_contract": Self::coverage_to_json(&engine_resp.coverage),
            "scope_file_count": engine_resp.scope_file_count,
        });

        // Backward-compat fields for existing MCP consumers.
        if engine_resp.triggered_lazy {
            response["parse_level"] = json!("structural");
            response["precise"] = json!(true);
        } else {
            response["parse_level"] = json!("manifest");
            response["precise"] = json!(false);
        }

        ctx.send_progress(1.0, &format!("Search complete ({} results)", hits.len()));

        let lr = LazyResponse::new("search", args);
        lr.with_precision_tier(engine_resp.precision_tier)
            .with_root_warnings(root_warnings)
            .with_lazy_warnings(engine_resp.warnings)
            .with_is_error(false)
            .build(response, self)
    }

    // ── handle_search_background ──────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn handle_search_background(
        &self,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: &str,
        is_manual_full: bool,
        include_roots: Vec<atlas_engine::IncludeRoot>,
        root_warnings: Vec<String>,
    ) -> (String, bool) {
        let task_id = self.async_state.task_manager.create_task("search", "search");
        let tid = task_id.clone();
        let store = self.store.clone();
        let project_root = self.project_root.clone();
        let task_manager = self.async_state.task_manager.clone();
        let q = query.to_string();
        let k = kind.map(|s| s.to_string());
        let sc = scope.to_string();
        let include_roots_strs: Vec<String> =
            include_roots.iter().map(|r| r.path.clone()).collect();
        let root_warnings_for_thread = root_warnings.clone();

        std::thread::spawn(move || {
            task_manager.update_progress(&tid, 5.0, "Starting scoped search...");

            let kind_filter = k.as_deref().and_then(SymbolKind::from_str);
            let analysis = if is_manual_full {
                SearchAnalysis::Manifest
            } else {
                SearchAnalysis::Auto
            };

            let req = ScopedSearchRequest {
                query: q.clone(),
                scope: Some(sc.clone()),
                kind: kind_filter,
                analysis,
                limit,
                include_roots: include_roots_strs,
                ..Default::default()
            };

            let engine: Arc<Engine> =
                Arc::new(Engine::from_store(store.clone(), Some(&project_root)));
            let svc = ScopedSearchService::new(store.clone(), engine.clone());

            let engine_resp = match svc.execute(req) {
                Ok(r) => r,
                Err(err) => {
                    task_manager.fail_task(&tid, &format!("Search error: {err}"));
                    return;
                }
            };

            let mut all_warnings = root_warnings_for_thread;
            all_warnings.extend(engine_resp.warnings.iter().cloned());

            let hits: Vec<SearchHit> = engine_resp
                .results
                .iter()
                .map(ToolRouter::search_result_to_hit)
                .collect();

            let mut response = json!({
                "query": q,
                "scope": sc,
                "results": hits,
                "total": engine_resp.total,
                "warnings": all_warnings,
                "precision_tier": engine_resp.precision_tier,
                "triggered_lazy": engine_resp.triggered_lazy,
                "analysis_contract": ToolRouter::coverage_to_json(&engine_resp.coverage),
            });

            response["scope_file_count"] = json!(engine_resp.scope_file_count);
            if engine_resp.triggered_lazy {
                response["parse_level"] = json!("structural");
                response["precise"] = json!(true);
            } else {
                response["parse_level"] = json!("manifest");
                response["precise"] = json!(false);
            }

            task_manager.complete_task(&tid, response);
        });

        (
            serde_json::to_string_pretty(&json!({
                "background": true,
                "task_id": task_id,
                "tool_name": "search",
                "method": "search",
                "status": "running",
                "progress": 0.0,
                "progress_message": "queued",
                "note": "Search is running in background. Poll task_status for progress percentages and completion."
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    // ── symbol detail ─────────────────────────────────────────────────────

    pub(crate) fn handle_symbol_detail(&mut self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "qualified_name");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "error": format!(
                        "qualified_name exceeds maximum length of {} characters",
                        MAX_SYMBOL_NAME_LENGTH
                    ),
                }))
                .unwrap_or_else(|e| e.to_string()),
                true,
            );
        }
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }

        let lr = LazyResponse::new("symbol", args);
        let query_id = lr.query_id().to_string();
        let resolution = self.resolve_symbol_input(
            &SymbolInput::Name(qname.to_string()),
            SymbolResolutionPolicy::UniqueOrCandidates,
        );
        let sym;
        let lazy_warnings;
        let structural_tier;
        let mut lazy_diag: Option<LazyDiagnostics> = None;
        match resolution {
            Ok(SymbolResolution::Single { symbol_id, .. }) => {
                // Found a unique symbol on the first try
                if let Ok(Some(s)) = self.store.find_symbol_by_id(&symbol_id) {
                    self.update_investigation(InvestigationFocus::Symbol(s.id));
                    let investigation = self.investigation_state.active_investigation.clone();
                    // Ensure structural data so caller/callee results
                    // include fresh edges from lazy extraction.
                    let outcome = self.ensure_structural_for_files(
                        [s.file_id],
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
                    // Re-query after lazy — structural replace may have
                    // updated symbol metadata or source ranges.
                    sym = match self.resolve_symbol_input(
                        &SymbolInput::Name(qname.to_string()),
                        SymbolResolutionPolicy::UniqueOrCandidates,
                    ) {
                        Ok(SymbolResolution::Single { symbol_id: new_id, .. }) => self
                            .store
                            .find_symbol_by_id(&new_id)
                            .unwrap_or_default()
                            .unwrap_or(s),
                        Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                            let amb_resp = Self::build_ambiguous_symbol_body(qname, &candidates);
                            return lr.with_is_error(true)
                                     .with_precision_tier(structural_tier)
                                     .with_lazy_warnings(lazy_warnings)
                                     .with_lazy_diag(lazy_diag)
                                     .build_with_args(amb_resp, args, self);
                        }
                        _ => s,
                    };
                } else {
                    let mut err = format!("Symbol not found: {qname}");
                    err.push_str(self.index_not_run_guidance());
                    return (err, true);
                }
            }
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                let amb_resp = Self::build_ambiguous_symbol_body(qname, &candidates);
                return lr.with_is_error(true)
                         .build_with_args(amb_resp, args, self);
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                // Not found in manifest — trigger lazy structural extraction
                let outcome = self.ensure_structural_for_symbol_name(
                    qname,
                    include_roots.clone(),
                    None,
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
                match self.resolve_symbol_input(
                    &SymbolInput::Name(qname.to_string()),
                    SymbolResolutionPolicy::UniqueOrCandidates,
                ) {
                    Ok(SymbolResolution::Single { symbol_id, .. }) => {
                        if let Ok(Some(s)) = self.store.find_symbol_by_id(&symbol_id) {
                            self.update_investigation(InvestigationFocus::Symbol(s.id));
                            sym = s;
                        } else {
                            let mut err = format!("Symbol not found: {qname}");
                            err.push_str(self.index_not_run_guidance());
                            return (err, true);
                        }
                    }
                    Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                        let amb_resp = Self::build_ambiguous_symbol_body(qname, &candidates);
                        return lr.with_is_error(true)
                                 .with_precision_tier(structural_tier)
                                 .with_lazy_warnings(lazy_warnings)
                                 .with_lazy_diag(lazy_diag)
                                 .build_with_args(amb_resp, args, self);
                    }
                    Ok(SymbolResolution::NotFound { .. }) => {
                        let mut err = format!("Symbol not found: {qname}");
                        err.push_str(self.index_not_run_guidance());
                        return (err, true);
                    }
                    Err(err) => {
                        return (err, true);
                    }
                }
            }
            Err(e) => return (e, true),
        };
        // Re-acquire graph after lazy structural may have refreshed it
        let se = match self.search_engine() {
            Ok(se) => se,
            Err(e) => return (format!("Internal error: {e}"), true),
        };
        let graph = se.graph_snapshot();
        let snap = graph.snapshot();

        let caller_nodes: Vec<_> = graph
            .callers(&sym.id)
            .callers
            .iter()
            .map(|&ix| self.node_json(snap, ix, None))
            .collect();
        let callee_nodes: Vec<_> = graph
            .callees(&sym.id)
            .callees
            .iter()
            .map(|&ix| self.node_json(snap, ix, None))
            .collect();

        let mut result = json!({
            "name": sym.name, "qualified_name": sym.qualified_name,
            "kind": sym.kind.as_str(), "language": sym.language.as_str(),
            "visibility": sym.visibility.as_ref().map(|v| v.as_str()), "signature": sym.signature,
            "file": self.resolve_file_path(&sym.file_id),
            "range": { "line": sym.range.start_line, "column": sym.range.start_column },
            "caller_count": caller_nodes.len(), "callee_count": callee_nodes.len(),
            "callers": caller_nodes, "callees": callee_nodes,
        });
        if include_code {
            if let Some(src) = self.read_symbol_source(&sym.id) {
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

        lr.with_precision_tier(structural_tier)
            .with_root_warnings(Vec::new()) // already merged via add_json_warnings above
            .with_lazy_warnings(Vec::new()) // already merged via add_json_warnings above
            .with_lazy_diag(lazy_diag)
            .with_analysis_contract(false) // symbol detail does not include analysis_contract
            .with_is_error(false)
            .build_with_args(result, &stored_args, self)
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Serialize [`SearchCoverage`] to JSON (the type does not derive Serialize).
    fn coverage_to_json(coverage: &SearchCoverage) -> serde_json::Value {
        match coverage {
            SearchCoverage::Full => json!({"level": "full"}),
            SearchCoverage::Partial { reason } => json!({"level": "partial", "reason": reason}),
        }
    }

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
    /// Callers should wrap this via LazyResponse.
    fn build_ambiguous_symbol_body(
        qname: &str,
        candidates: &[ScoredCandidate],
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
        let hint = format!(
            "Symbol '{}' is ambiguous ({} matches). Use the symbol_ref from a candidate below.",
            qname,
            candidates.len()
        );
        json!({
            "ok": false,
            "error": hint,
            "candidates": candidates_json,
        })
    }
}
