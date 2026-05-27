//! Search tools: scoped symbol search and single-symbol lookup.
//!
//! Search is intentionally store-backed and scope-required so ordinary MCP
//! search calls do not build the whole graph snapshot or trigger unbounded
//! extraction on large repositories.

use atlas_engine::LazyStructuralService;
use atlas_engine::Store;
use atlas_engine::SymbolDef;
use atlas_engine::SymbolKind;

use super::{ToolRouter, get_str, get_str_opt, get_u64};

use serde_json::json;
use std::sync::Arc;

const SYNC_STRUCTURAL_SCOPE_FILE_LIMIT: usize = 8;
const LIKE_FALLBACK_SCOPE_FILE_LIMIT: usize = 32;
const PREHEAT_SCOPE_FILE_LIMIT: usize = 64;
const PREHEAT_FILE_LIMIT: usize = 8;
const SEARCH_CANDIDATE_MULTIPLIER: usize = 4;
/// Maximum total indexed files for which scope="." automatically gets full
/// structural parsing regardless of scope file count.  Applies to fully-indexed
/// small projects (≤ 200 files total in the index).
const SMALL_PROJECT_FULL_SCOPE_LIMIT: usize = 200;

#[derive(Debug, Clone, serde::Serialize)]
struct SearchHit {
    name: String,
    qualified_name: String,
    kind: String,
    language: String,
    score: f64,
    file: String,
    layer: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ScopedSearchResponse {
    query: String,
    scope: String,
    scope_file_count: usize,
    parse_level: &'static str,
    precise: bool,
    results: Vec<SearchHit>,
    warnings: Vec<String>,
    background_preparse: Option<String>,
}

impl ToolRouter {
    pub(crate) fn send_progress(&self, percent: f64, message: &str) {
        if let Some(ref sender) = self.progress_sender {
            let _ = sender.send((percent, Some(1.0), Some(message.to_string())));
        }
    }

    pub(crate) fn handle_search(&mut self, args: &serde_json::Value) -> (String, bool) {
        let query = get_str(args, "query");
        let limit = (get_u64(args, "limit").unwrap_or(20) as usize).min(200);
        let kind = get_str_opt(args, "kind");
        let scope = get_str_opt(args, "scope")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // When a manual full structural index exists (built via CLI `atlas index`),
        // scope restrictions are lifted and lazy structural is disabled — all
        // files already have complete structural facts.
        let is_manual_full = self.has_manual_full_index();

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
            return self.handle_search_background(query, limit, kind, &scope, is_manual_full);
        }
        self.handle_search_sync(query, limit, kind, &scope, is_manual_full)
    }

    fn handle_search_sync(
        &self,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: &str,
        is_manual_full: bool,
    ) -> (String, bool) {
        self.send_progress(0.1, &format!("Searching for '{}' in {}...", query, scope));
        if !self.has_indexed_files() {
            return (
                "No indexed files found — please run 'index' tool first.".into(),
                true,
            );
        }

        let response = match execute_scoped_search(
            self.store.clone(),
            self.project_root.clone(),
            query,
            limit,
            kind,
            scope,
            is_manual_full,
            Some(|percent, message: String| self.send_progress(percent, &message)),
        ) {
            Ok(r) => r,
            Err(err) => {
                let mut s = format!("Search error: {}", err);
                s.push_str(self.index_not_run_guidance());
                return (s, true);
            }
        };

        self.send_progress(
            1.0,
            &format!("Search complete ({} results)", response.results.len()),
        );
        (
            serde_json::to_string_pretty(&response).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    fn handle_search_background(
        &self,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: &str,
        is_manual_full: bool,
    ) -> (String, bool) {
        let task_id = self.task_manager.create_task("search", "search");
        let tid = task_id.clone();
        let store = self.store.clone();
        let project_root = self.project_root.clone();
        let task_manager = self.task_manager.clone();
        let q = query.to_string();
        let k = kind.map(|s| s.to_string());
        let sc = scope.to_string();

        std::thread::spawn(move || {
            task_manager.update_progress(&tid, 5.0, "Starting scoped search...");
            let response = match execute_scoped_search(
                store,
                project_root,
                &q,
                limit,
                k.as_deref(),
                &sc,
                is_manual_full,
                Some(|percent, message: String| {
                    task_manager.update_progress(&tid, percent * 100.0, &message)
                }),
            ) {
                Ok(r) => r,
                Err(err) => {
                    task_manager.fail_task(&tid, &format!("Search error: {}", err));
                    return;
                }
            };
            task_manager.complete_task(
                &tid,
                serde_json::to_value(&response)
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() })),
            );
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

    /// Trigger lazy structural extraction for the given query.  When a manual
    /// full index already exists, this is a no-op — all files already have
    /// complete structural data.
    fn try_lazy_structural(&mut self, query: &str) {
        // Manual full index: structural data already complete — skip lazy extraction.
        if self.has_manual_full_index() {
            return;
        }
        let lazy = LazyStructuralService::new(self.store.clone(), Some(self.project_root.clone()));
        let _ = lazy.ensure_structural_for_symbol(query);
    }

    pub(crate) fn handle_symbol(&mut self, args: &serde_json::Value) -> (String, bool) {
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
            "file": self.resolve_file_path(&sym.file_id),
            "range": { "line": sym.range.start_line, "column": sym.range.start_column },
            "callers": graph.callers(&sym.id).callers.len(), "callees": graph.callees(&sym.id).callees.len(),
        })).unwrap_or_else(|e| e.to_string()), false)
    }
}

fn execute_scoped_search<F>(
    store: Arc<Store>,
    project_root: std::path::PathBuf,
    query: &str,
    limit: usize,
    kind: Option<&str>,
    scope: &str,
    is_manual_full: bool,
    progress: Option<F>,
) -> anyhow::Result<ScopedSearchResponse>
where
    F: Fn(f64, String),
{
    let normalized_scope = normalize_scope(scope);
    let scope_file_count = store.count_files_in_scope(&normalized_scope)?;
    let mut warnings = Vec::new();
    let mut background_preparse = None;
    let kind_filter = kind.and_then(SymbolKind::from_str);

    if scope_file_count == 0 {
        warnings.push(format!(
            "Scope '{}' has no indexed files. Run index first or choose a different project-relative scope.",
            normalized_scope
        ));
        return Ok(ScopedSearchResponse {
            query: query.to_string(),
            scope: normalized_scope,
            scope_file_count,
            parse_level: "none",
            precise: false,
            results: Vec::new(),
            warnings,
            background_preparse,
        });
    }

    // For fully-indexed small projects (≤ 200 total indexed files), treat the
    // entire project as "small enough" for structural parsing regardless of the
    // scope's individual file count.  This avoids the confusing situation where
    // scope="." on a 99-file project is rejected.
    let total_indexed = store.count_files().unwrap_or(0);
    let is_small_project = total_indexed > 0 && total_indexed <= SMALL_PROJECT_FULL_SCOPE_LIMIT;

    // When a manual full index exists, all files already have structural data.
    // Skip the file-count-based heuristic entirely — every scope is "precise"
    // because lazy structural has nothing to do.
    let precise = if is_manual_full {
        true
    } else {
        scope_file_count <= SYNC_STRUCTURAL_SCOPE_FILE_LIMIT
            || (is_small_project && scope_file_count == total_indexed)
    };
    let parse_level = if precise { "structural" } else { "manifest" };

    if precise && !is_manual_full {
        if let Some(ref progress) = progress {
            progress(
                0.35,
                "Scope is small enough; ensuring structural index...".to_string(),
            );
        }
        let max_files = scope_file_count.max(SYNC_STRUCTURAL_SCOPE_FILE_LIMIT);
        let file_ids =
            store.list_file_ids_in_scope(&normalized_scope, max_files)?;
        let lazy = LazyStructuralService::new(store.clone(), Some(project_root.clone()));
        match lazy.ensure_structural_for_file_ids(&file_ids) {
            Ok(result) => {
                if result.budget_exceeded {
                    warnings.push(
                        "Structural parsing hit the per-query budget; results may still be partial. Narrow the scope for exact parsing."
                            .into(),
                    );
                }
            }
            Err(err) => warnings.push(format!(
                "Structural parsing failed for scoped search; returning indexed symbols only: {err:#}"
            )),
        }
    } else if !is_manual_full {
        // Single actionable warning — no contradictory "returning manifest results"
        // when results may actually be empty.
        let level_desc = if scope_file_count <= LIKE_FALLBACK_SCOPE_FILE_LIMIT {
            "index search with fuzzy fallback"
        } else {
            "exact index search (fuzzy matching disabled for large scopes)"
        };
        warnings.push(format!(
            "Scope has {} files (precise structural parsing: ≤{}{}). Using {}. For best results, narrow scope to a specific directory like 'src/' or 'internal/'.",
            scope_file_count,
            SYNC_STRUCTURAL_SCOPE_FILE_LIMIT,
            if is_small_project {
                format!(", full-project: ≤{}", SMALL_PROJECT_FULL_SCOPE_LIMIT)
            } else {
                String::new()
            },
            level_desc,
        ));
    }

    if let Some(ref progress) = progress {
        progress(0.70, "Running scoped symbol query...".to_string());
    }
    let mut symbols = search_symbols_scoped(
        &store,
        query,
        &normalized_scope,
        limit,
        kind_filter,
        scope_file_count,
    )?;
    symbols.truncate(limit);

    // Empty result guidance: when no results and not in precise mode, help the
    // agent understand why and what to try next.
    if symbols.is_empty() && !precise {
        warnings.push(format!(
            "Search for '{}' returned no results in scope '{}'. Possible causes: (1) symbol not yet structurally parsed — narrow scope to the file; (2) no exact match — try a broader query or use 'atlas_status' to confirm indexing coverage.",
            query, normalized_scope
        ));
    }

    // Background preparse: skipped when manual full index exists (nothing to preparse).
    if !is_manual_full {
        let result_file_ids: Vec<_> = symbols
            .iter()
            .map(|sym| sym.file_id)
            .take(PREHEAT_FILE_LIMIT)
            .collect();
        if !precise && scope_file_count <= PREHEAT_SCOPE_FILE_LIMIT && !result_file_ids.is_empty() {
            spawn_preparse(store.clone(), project_root, result_file_ids);
            background_preparse = Some(format!(
                "Scheduled structural preparse for up to {} result-adjacent files.",
                PREHEAT_FILE_LIMIT
            ));
        } else if !precise && scope_file_count > PREHEAT_SCOPE_FILE_LIMIT {
            warnings.push(format!(
                "Background structural preparse skipped because scope has more than {} files; narrow scope to enable preparse.",
                PREHEAT_SCOPE_FILE_LIMIT
            ));
        }
    }

    let results = symbols
        .into_iter()
        .map(|sym| symbol_hit(&store, query, sym))
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(ScopedSearchResponse {
        query: query.to_string(),
        scope: normalized_scope,
        scope_file_count,
        parse_level,
        precise,
        results,
        warnings,
        background_preparse,
    })
}

fn search_symbols_scoped(
    store: &Store,
    query: &str,
    scope: &str,
    limit: usize,
    kind_filter: Option<SymbolKind>,
    scope_file_count: usize,
) -> anyhow::Result<Vec<SymbolDef>> {
    let candidate_limit = limit
        .saturating_mul(SEARCH_CANDIDATE_MULTIPLIER)
        .max(50)
        .min(1000);

    let mut symbols =
        store.find_symbols_by_name_in_scope(query, scope, candidate_limit, kind_filter.as_ref())?;
    if symbols.is_empty() {
        symbols = store.search_symbols_in_scope_with_limit(
            query,
            scope,
            candidate_limit,
            kind_filter.as_ref(),
        )?;
    }
    if symbols.is_empty() && query.len() >= 2 && scope_file_count <= LIKE_FALLBACK_SCOPE_FILE_LIMIT
    {
        symbols = store.search_symbols_by_name_like_in_scope(
            query,
            scope,
            None,
            candidate_limit,
            kind_filter.as_ref(),
        )?;
    }
    symbols.sort_by(|a, b| {
        score_symbol(query, b)
            .partial_cmp(&score_symbol(query, a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(symbols)
}

fn symbol_hit(store: &Store, query: &str, sym: SymbolDef) -> anyhow::Result<SearchHit> {
    let file = store
        .get_file(&sym.file_id)?
        .map(|f| f.path)
        .unwrap_or_default();
    let score = score_symbol(query, &sym);
        Ok(SearchHit {
        name: sym.name,
        qualified_name: sym.qualified_name,
        kind: sym.kind.as_str().to_string(),
        language: sym.language.as_str().to_string(),
        score,
        file,
        layer: sym.layer,
    })
}

fn score_symbol(query: &str, sym: &SymbolDef) -> f64 {
    let q = query.to_lowercase();
    let name = sym.name.to_lowercase();
    let qname = sym.qualified_name.to_lowercase();
    let name_score = if name == q {
        1.0
    } else if name.starts_with(&q) {
        0.9
    } else if name.contains(&q) {
        0.75
    } else if qname.contains(&q) {
        0.6
    } else {
        0.35
    };
    let kind_bonus = match sym.kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => 0.08,
        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait => 0.06,
        _ => 0.0,
    };
    name_score + kind_bonus
}

fn normalize_scope(scope: &str) -> String {
    scope
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

fn spawn_preparse(
    store: Arc<Store>,
    project_root: std::path::PathBuf,
    file_ids: Vec<atlas_engine::FileId>,
) {
    std::thread::spawn(move || {
        let lazy = LazyStructuralService::new(store, Some(project_root));
        let _ = lazy.ensure_structural_for_file_ids(&file_ids);
    });
}
