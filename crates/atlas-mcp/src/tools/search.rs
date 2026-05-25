//! Search tools: full-text symbol search and single-symbol lookup.
//! Supports `background: true` for async execution via task_status polling.

use atlas_engine::LazyStructuralService;
use atlas_engine::SymbolKind;

use super::{ToolRouter, get_str, get_str_opt, get_u64};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn send_progress(&self, percent: f64, message: &str) {
        if let Some(ref sender) = self.progress_sender {
            let _ = sender.send((percent, Some(1.0), Some(message.to_string())));
        }
    }

    pub(crate) fn handle_search(&self, args: &serde_json::Value) -> (String, bool) {
        let query = get_str(args, "query");
        let limit = (get_u64(args, "limit").unwrap_or(20) as usize).min(200);
        let kind = get_str_opt(args, "kind");
        let scope = get_str_opt(args, "scope");
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if background {
            return self.handle_search_background(query, limit, kind, scope);
        }
        self.handle_search_sync(query, limit, kind, scope)
    }

    fn handle_search_sync(
        &self,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: Option<&str>,
    ) -> (String, bool) {
        let scope_display = scope.unwrap_or("(all)");
        self.send_progress(
            0.1,
            &format!("Searching for '{}' in {}...", query, scope_display),
        );
        let entries = match self.do_search(query, limit, kind) {
            Ok(e) => self.filter_by_scope(e, scope),
            Err(err) => {
                let mut s = format!("Search error: {}", err);
                s.push_str(self.index_not_run_guidance());
                return (s, true);
            }
        };
        if entries.is_empty() && !self.has_indexed_files() {
            return (
                "No indexed files found — please run 'index' tool first.".into(),
                true,
            );
        }
        let needs_lazy = entries.is_empty() || entries.iter().any(|e| e.symbol.layer == "manifest");
        let final_entries = if needs_lazy {
            self.send_progress(0.5, "Extracting structural data...");
            self.try_lazy_structural(query);
            match self.do_search(query, limit, kind) {
                Ok(e) => self.filter_by_scope(e, scope),
                Err(err) => {
                    let mut s = format!("Search error after extraction: {}", err);
                    s.push_str(self.index_not_run_guidance());
                    return (s, true);
                }
            }
        } else {
            entries
        };
        self.send_progress(
            1.0,
            &format!("Search complete ({} results)", final_entries.len()),
        );
        (self.format_search_results(query, &final_entries), false)
    }

    fn filter_by_scope(
        &self,
        entries: Vec<atlas_engine::SearchResult>,
        scope: Option<&str>,
    ) -> Vec<atlas_engine::SearchResult> {
        let Some(scope) = scope else {
            return entries;
        };
        let scope = scope.trim_end_matches('/');
        entries
            .into_iter()
            .filter(|e| e.file_path.as_ref().map_or(true, |p| p.starts_with(scope)))
            .collect()
    }

    fn handle_search_background(
        &self,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: Option<&str>,
    ) -> (String, bool) {
        let task_id = self.task_manager.create_task("search", "search");
        let tid = task_id.clone();
        let store = self.store.clone();
        let project_root = self.project_root.clone();
        let task_manager = self.task_manager.clone();
        let q = query.to_string();
        let k = kind.map(|s| s.to_string());
        let sc = scope.map(|s| s.to_string());

        std::thread::spawn(move || {
            let graph = atlas_engine::GraphEngine::from_store(&store, 0.3).unwrap();
            let search_engine =
                atlas_engine::SearchEngine::new(store.clone(), std::sync::Arc::new(graph));
            let do_search =
                |q: &str, k: Option<&str>| -> anyhow::Result<Vec<atlas_engine::SearchResult>> {
                    if let Some(k_str) = k {
                        SymbolKind::from_str(k_str)
                            .map(|sk| search_engine.search_by_kind(q, sk, limit))
                            .unwrap_or_else(|| search_engine.search_simple(q, limit))
                    } else {
                        search_engine.search_simple(q, limit)
                    }
                };
            let entries = match do_search(&q, k.as_deref()) {
                Ok(e) => e,
                Err(err) => {
                    task_manager.fail_task(&tid, &format!("Search error: {}", err));
                    return;
                }
            };
            let needs_lazy =
                entries.is_empty() || entries.iter().any(|e| e.symbol.layer == "manifest");
            let final_entries = if needs_lazy {
                task_manager.update_progress(&tid, 50.0, "Extracting structural data...");
                let lazy = LazyStructuralService::new(store.clone(), Some(project_root));
                if let Ok(result) = lazy.ensure_structural_for_symbol(&q) {
                    if result.files_built > 0 {
                        task_manager.update_progress(
                            &tid,
                            80.0,
                            &format!("Extracted {} files", result.files_built),
                        );
                    }
                }
                match do_search(&q, k.as_deref()) {
                    Ok(e) => e,
                    Err(err) => {
                        task_manager.fail_task(&tid, &format!("Re-search failed: {}", err));
                        return;
                    }
                }
            } else {
                entries
            };
            let final_entries = if let Some(ref s) = sc {
                let s = s.trim_end_matches('/');
                final_entries
                    .into_iter()
                    .filter(|e| e.file_path.as_ref().map_or(true, |p| p.starts_with(s)))
                    .collect()
            } else {
                final_entries
            };
            task_manager.complete_task(
                &tid,
                json!({
                    "query": q, "count": final_entries.len(),
                    "results": final_entries.iter().map(|e| json!({
                        "name": e.symbol.name, "qualified_name": e.symbol.qualified_name,
                        "kind": e.symbol.kind.as_str(), "language": e.symbol.language.as_str(),
                        "score": e.score.total, "file": e.file_path.as_deref().unwrap_or(""),
                        "file_id": e.symbol.file_id.short_hex(),
                    })).collect::<Vec<_>>(),
                }),
            );
        });

        (
            serde_json::to_string_pretty(&json!({
                "background": true,
                "task_id": task_id,
                "tool_name": "search",
                "method": "search",
                "status": "running",
                "progress": null,
                "note": "Search is running in background. Use task_status to check completion."
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    fn do_search(
        &self,
        query: &str,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<atlas_engine::SearchResult>> {
        if let Some(k_str) = kind {
            SymbolKind::from_str(k_str)
                .map(|k| self.search_engine().search_by_kind(query, k, limit))
                .unwrap_or_else(|| self.search_engine().search_simple(query, limit))
        } else {
            self.search_engine().search_simple(query, limit)
        }
    }

    fn try_lazy_structural(&self, query: &str) {
        let lazy = LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
        let _ = lazy.ensure_structural_for_symbol(query);
    }

    fn format_search_results(&self, query: &str, entries: &[atlas_engine::SearchResult]) -> String {
        serde_json::to_string_pretty(&json!({
            "query": query, "count": entries.len(),
            "results": entries.iter().map(|e| json!({
                "name": e.symbol.name, "qualified_name": e.symbol.qualified_name,
                "kind": e.symbol.kind.as_str(), "language": e.symbol.language.as_str(),
                "score": e.score.total, "file": e.file_path.as_deref().unwrap_or(""),
                "file_id": e.symbol.file_id.short_hex(),
            })).collect::<Vec<_>>(),
        }))
        .unwrap_or_else(|e| e.to_string())
    }

    pub(crate) fn handle_symbol(&self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "qualified_name");
        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => {
                let mut s = format!("Lookup error: {}", e);
                s.push_str(self.index_not_run_guidance());
                return (s, true);
            }
        };
        let sym = match symbols.into_iter().next() {
            Some(s) => s,
            None => {
                self.try_lazy_structural(qname);
                let retry = self.store.find_symbols_by_qname(qname).unwrap_or_default();
                match retry.into_iter().next() {
                    Some(s) => s,
                    None => {
                        let mut s = format!("Symbol not found: {}", qname);
                        s.push_str(self.index_not_run_guidance());
                        return (s, true);
                    }
                }
            }
        };
        let graph = self.search_engine().graph_snapshot();
        (serde_json::to_string_pretty(&json!({
            "name": sym.name, "qualified_name": sym.qualified_name,
            "kind": sym.kind.as_str(), "language": sym.language.as_str(),
            "visibility": sym.visibility.as_ref().map(|v| v.as_str()), "signature": sym.signature,
            "file": self.resolve_file_path(&sym.file_id), "file_id": sym.file_id.short_hex(),
            "range": { "line": sym.range.start_line, "column": sym.range.start_column },
            "callers": graph.callers(&sym.id).callers.len(), "callees": graph.callees(&sym.id).callees.len(),
        })).unwrap_or_else(|e| e.to_string()), false)
    }
}
