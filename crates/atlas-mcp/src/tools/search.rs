//! Search tools: scoped symbol search and single-symbol lookup.
//!
//! Search is intentionally store-backed and scope-required so ordinary MCP
//! search calls do not build the whole graph snapshot or trigger unbounded
//! extraction on large repositories.

use atlas_engine::FileId;
use atlas_engine::InvestigationFocus;
use atlas_engine::LazyOrchestrator;
use atlas_engine::LazyPolicy;
use atlas_engine::Store;
use atlas_engine::SymbolDef;
use atlas_engine::SymbolKind;

use super::lazy_refresh::LazyRefreshQueue;
use super::lazy_response::LazyDiagnostics;
use super::query_snapshot::{QuerySnapshot, QueryStatus};
use super::{
    MAX_QUERY_LENGTH, MAX_SYMBOL_NAME_LENGTH, ToolRouter, add_json_warnings, get_str, get_str_opt,
    get_u64,
};

use crate::task_manager::TaskManager;

use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

const SYNC_STRUCTURAL_SCOPE_FILE_LIMIT: usize = 8;
const LIKE_FALLBACK_SCOPE_FILE_LIMIT: usize = 32;
const PREHEAT_SCOPE_FILE_LIMIT: usize = 64;
const PREHEAT_FILE_LIMIT: usize = 8;
const SEARCH_CANDIDATE_MULTIPLIER: usize = 4;
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

#[derive(Debug, Clone, serde::Serialize)]
struct ScopedSearchResponse {
    query: String,
    scope: String,
    scope_file_count: usize,
    parse_level: &'static str,
    precise: bool,
    results: Vec<SearchHit>,
    warnings: Vec<String>,
    background_preparse: Option<serde_json::Value>,
    /// Precision tier of the structural extraction (only set for precise searches).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    precision_tier: Option<atlas_engine::structs::precision::PrecisionTier>,
    /// Internal: file IDs built during lazy structural extraction.
    /// The caller should refresh the in-memory graph with these.
    #[serde(skip)]
    built_file_ids: Vec<FileId>,
    /// Unified lazy extraction diagnostics (None when no lazy extraction ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lazy_diagnostics: Option<LazyDiagnostics>,
}

impl ToolRouter {
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
        let (result_str, is_err, built_file_ids) = self.handle_search_sync(
            ctx,
            query,
            limit,
            kind,
            &scope,
            is_manual_full,
            include_roots,
            root_warnings,
        );
        self.lazy_refresh_queue.record_lazy_writes(&built_file_ids);
        let _ = self.maybe_refresh_graph();
        (result_str, is_err)
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_search_sync(
        &self,
        ctx: &super::ToolCallContext,
        query: &str,
        limit: usize,
        kind: Option<&str>,
        scope: &str,
        is_manual_full: bool,
        include_roots: Vec<atlas_engine::IncludeRoot>,
        root_warnings: Vec<String>,
    ) -> (String, bool, Vec<FileId>) {
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
                Vec::new(),
            );
        }

        let progress_sender = ctx.progress_sender.clone();
        let response = match execute_scoped_search(
            self.task_manager.clone(),
            self.store.clone(),
            self.project_root.clone(),
            query,
            limit,
            kind,
            scope,
            is_manual_full,
            include_roots,
            root_warnings,
            Arc::clone(&self.lazy_refresh_queue),
            Some(move |percent, message: String| {
                if let Some(ref sender) = progress_sender {
                    let _ = sender.send((percent, Some(1.0), Some(message)));
                }
            }),
        ) {
            Ok(r) => r,
            Err(err) => {
                let mut s = format!("Search error: {err}");
                s.push_str(self.index_not_run_guidance());
                return (s, true, Vec::new());
            }
        };

        let built_file_ids = response.built_file_ids.clone();
        ctx.send_progress(
            1.0,
            &format!("Search complete ({} results)", response.results.len()),
        );
        (
            serde_json::to_string_pretty(&response).unwrap_or_else(|e| e.to_string()),
            false,
            built_file_ids,
        )
    }

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
        let task_id = self.task_manager.create_task("search", "search");
        let tid = task_id.clone();
        let store = self.store.clone();
        let project_root = self.project_root.clone();
        let task_manager = self.task_manager.clone();
        let lazy_refresh_queue = Arc::clone(&self.lazy_refresh_queue);
        let q = query.to_string();
        let k = kind.map(|s| s.to_string());
        let sc = scope.to_string();
        let roots_for_thread = include_roots.clone();
        let root_warnings_for_thread = root_warnings.clone();

        let bg_task_manager = task_manager.clone();
        std::thread::spawn(move || {
            task_manager.update_progress(&tid, 5.0, "Starting scoped search...");
            let response = match execute_scoped_search(
                bg_task_manager,
                store,
                project_root,
                &q,
                limit,
                k.as_deref(),
                &sc,
                is_manual_full,
                roots_for_thread,
                root_warnings_for_thread,
                Arc::clone(&lazy_refresh_queue),
                Some(|percent, message: String| {
                    task_manager.update_progress(&tid, percent * 100.0, &message)
                }),
            ) {
                Ok(r) => r,
                Err(err) => {
                    task_manager.fail_task(&tid, &format!("Search error: {err}"));
                    return;
                }
            };
            if !response.built_file_ids.is_empty() {
                lazy_refresh_queue.signal_background_writes();
            }
            let json_response = serde_json::to_value(&response)
                .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
            task_manager.complete_task(&tid, json_response);
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

        let query_id = Self::generate_query_id();
        let symbols = match self.store.find_symbols_by_qname(qname) {
            Ok(s) => s,
            Err(e) => {
                let mut s = format!("Lookup error: {e}");
                s.push_str(self.index_not_run_guidance());
                return (s, true);
            }
        };
        let sym;
        let lazy_warnings;
        let structural_tier;
        let mut lazy_diag: Option<LazyDiagnostics> = None;
        match symbols.into_iter().next() {
            Some(s) => {
                // Update investigation with the known symbol
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
                    lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                }
                // Re-query after lazy — structural replace may have
                // updated symbol metadata or source ranges.
                sym = self
                    .store
                    .find_symbols_by_qname(qname)
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .unwrap_or(s);
            }
            None => {
                let outcome = self.ensure_structural_for_symbol_name(
                    qname,
                    include_roots.clone(),
                    None,
                    Some(&query_id),
                );
                lazy_warnings = outcome.warnings;
                structural_tier = outcome.precision_tier;
                if let Some(ref lo) = outcome.lazy_outcome {
                    lazy_diag = Some(LazyDiagnostics::from_structural(lo));
                }
                let retry = self.store.find_symbols_by_qname(qname).unwrap_or_default();
                match retry.into_iter().next() {
                    Some(s) => {
                        self.update_investigation(InvestigationFocus::Symbol(s.id));
                        sym = s;
                    }
                    None => {
                        let mut s = format!("Symbol not found: {qname}");
                        s.push_str(self.index_not_run_guidance());
                        return (s, true);
                    }
                }
            }
        };
        // Re-acquire graph after lazy structural may have refreshed it
        let graph = self.search_engine().graph_snapshot();
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

        use atlas_engine::structs::precision::PrecisionTier;
        result["precision_tier"] = serde_json::to_value(structural_tier).unwrap_or(json!(null));
        if structural_tier != PrecisionTier::Exact {
            if let Some(hint) = atlas_engine::precision::next_action_structural(structural_tier) {
                result["hint"] = json!(hint);
            }
        }
        if let Some(ref diag) = lazy_diag {
            result["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
        }
        result["query_id"] = json!(query_id);

        let mut stored_args = args.clone();
        if let Some(obj) = stored_args.as_object_mut() {
            obj.insert("view".into(), serde_json::Value::String("detail".into()));
        }
        self.store_snapshot(QuerySnapshot {
            query_id: query_id.clone(),
            tool_name: "symbol".into(),
            tool_args: stored_args,
            lazy_window: None,
            created_at: Instant::now(),
            status: if structural_tier == PrecisionTier::Exact {
                QueryStatus::Ready
            } else {
                QueryStatus::Partial
            },
        });

        (
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_scoped_search<F>(
    task_manager: Arc<TaskManager>,
    store: Arc<Store>,
    project_root: std::path::PathBuf,
    query: &str,
    limit: usize,
    kind: Option<&str>,
    scope: &str,
    is_manual_full: bool,
    include_roots: Vec<atlas_engine::IncludeRoot>,
    root_warnings: Vec<String>,
    lazy_refresh_queue: Arc<LazyRefreshQueue>,
    progress: Option<F>,
) -> anyhow::Result<ScopedSearchResponse>
where
    F: Fn(f64, String),
{
    use atlas_engine::structs::precision::PrecisionTier;

    let normalized_scope = normalize_scope(scope);
    let scope_file_count = store.count_files_in_scope(&normalized_scope)?;
    let mut warnings: Vec<String> = root_warnings;
    let mut background_preparse = None;
    let mut built_file_ids: Vec<FileId> = Vec::new();
    let mut precision_tier: Option<PrecisionTier> = None;
    let mut lazy_diagnostics: Option<LazyDiagnostics> = None;
    let kind_filter = kind.and_then(SymbolKind::from_str);

    if scope_file_count == 0 {
        warnings.push(format!(
            "Scope '{normalized_scope}' has no indexed files. Run index first or choose a different project-relative scope."
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
            built_file_ids: Vec::new(),
            precision_tier: None,
            lazy_diagnostics: None,
        });
    }

    // When a manual full index exists, all files already have structural data.
    // Skip the file-count-based heuristic entirely — every scope is "precise"
    // because lazy structural has nothing to do.
    let precise = if is_manual_full {
        true
    } else {
        scope_file_count <= SYNC_STRUCTURAL_SCOPE_FILE_LIMIT
    };
    let parse_level = if precise { "structural" } else { "manifest" };

    let mut symbols = search_symbols_scoped(
        &store,
        query,
        &normalized_scope,
        limit,
        kind_filter,
        scope_file_count,
    )?;

    if precise && !is_manual_full {
        if let Some(ref progress) = progress {
            progress(
                0.35,
                "Ensuring structural index for search candidates...".to_string(),
            );
        }
        let mut seen_files = HashSet::new();
        let mut file_ids: Vec<FileId> = symbols
            .iter()
            .filter_map(|sym| {
                if seen_files.insert(sym.file_id) {
                    Some(sym.file_id)
                } else {
                    None
                }
            })
            .take(SYNC_STRUCTURAL_SCOPE_FILE_LIMIT)
            .collect();

        if file_ids.is_empty() {
            file_ids = store
                .list_file_ids_in_scope(&normalized_scope, SYNC_STRUCTURAL_SCOPE_FILE_LIMIT)?;
        }

        let mut total_budget_exceeded = false;
        let orchestrator = LazyOrchestrator::new(
            store.clone(),
            Some(project_root.clone()),
            include_roots.clone(),
        );
        match orchestrator.ensure_structural_for_files(
            &file_ids,
            LazyPolicy::ForegroundStructural,
            None,
            None,
        ) {
            Ok(outcome) => {
                total_budget_exceeded = outcome.budget_exceeded;
                lazy_diagnostics = Some(LazyDiagnostics::from_structural(&outcome));
                built_file_ids = outcome.built_file_ids;
                precision_tier = Some(outcome.precision_tier);
            }
            Err(err) => {
                warnings.push(format!("Structural parsing failed: {err:#}"));
            }
        }
        if total_budget_exceeded {
            warnings
                .push("Structural parsing hit budget; narrow the scope for exact results.".into());
        }

        if let Some(ref progress) = progress {
            progress(
                0.60,
                "Re-running scoped symbol query after structural parsing...".to_string(),
            );
        }
        symbols = search_symbols_scoped(
            &store,
            query,
            &normalized_scope,
            limit,
            kind_filter,
            scope_file_count,
        )?;
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
            String::new(),
            level_desc,
        ));
    }

    if let Some(ref progress) = progress {
        progress(0.70, "Running scoped symbol query...".to_string());
    }
    symbols.truncate(limit);

    // Empty result guidance: when no results and not in precise mode, help the
    // agent understand why and what to try next.
    if symbols.is_empty() && !precise {
        warnings.push(format!(
            "Search for '{query}' returned no results in scope '{normalized_scope}'. Possible causes: (1) symbol not yet structurally parsed — narrow scope to the file; (2) no exact match — try a broader query or use 'status' to confirm indexing coverage."
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
            let preparse_task_id = spawn_preparse(
                task_manager,
                store.clone(),
                project_root,
                result_file_ids,
                include_roots.clone(),
                lazy_refresh_queue.clone(),
            );
            background_preparse = Some(serde_json::json!({
                "task_id": preparse_task_id,
                "status": "pending",
                "file_limit": PREHEAT_FILE_LIMIT,
            }));
        } else if !precise && scope_file_count > PREHEAT_SCOPE_FILE_LIMIT {
            warnings.push(format!(
                "Background structural preparse skipped because scope has more than {PREHEAT_SCOPE_FILE_LIMIT} files; narrow scope to enable preparse."
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
        built_file_ids,
        precision_tier,
        lazy_diagnostics,
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
        .clamp(50, 1000);

    // When scope is empty (project root / "."), use non-scoped search
    // functions — scoped equivalents treat empty scope as "no files".
    if scope.is_empty() {
        let mut symbols = store.find_symbols_by_name(query)?;
        if symbols.is_empty() {
            symbols = store.search_symbols(query)?;
        }
        symbols.truncate(candidate_limit);
        if symbols.is_empty() && query.len() >= 2 {
            symbols = store.search_symbols_by_name_like(
                query,
                None,
                candidate_limit,
                kind_filter.as_ref(),
            )?;
        }
        // Filter by kind if specified (non-scoped methods don't all support kind_filter)
        if let Some(kind) = kind_filter {
            symbols.retain(|s| s.kind == kind);
        }
        symbols.sort_by(|a, b| {
            score_symbol(query, b)
                .partial_cmp(&score_symbol(query, a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        return Ok(symbols);
    }

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
            // Tie-break by qualified_name for deterministic ordering
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
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
        line: sym.range.start_line,
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

/// Normalize a scope path for database lookups: strip `./`, `/` prefixes and
/// `/` suffixes.  `"."` (project root) normalizes to `""` so the db counts all
/// files.
fn normalize_scope(scope: &str) -> String {
    let s = scope.trim();
    // "." is the project root — normalize to empty string so store methods
    // treat it as "all files" where supported (count_files_in_scope).
    if s == "." {
        return String::new();
    }
    s.trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

fn spawn_preparse(
    task_manager: Arc<TaskManager>,
    store: Arc<Store>,
    project_root: std::path::PathBuf,
    file_ids: Vec<atlas_engine::FileId>,
    include_roots: Vec<atlas_engine::IncludeRoot>,
    lazy_refresh_queue: Arc<LazyRefreshQueue>,
) -> String {
    let task_id = task_manager.create_task("search", "preparse");
    let tid = task_id.clone();
    let tm = Arc::clone(&task_manager);

    std::thread::spawn(move || {
        // Use LazyOrchestrator for closure-aware lazy structural
        // so preparsed results are consistent with sync search results.
        let orchestrator = atlas_engine::LazyOrchestrator::new(
            store.clone(),
            Some(project_root.clone()),
            include_roots,
        );
        let outcome = orchestrator.ensure_structural_for_files(
            &file_ids,
            atlas_engine::LazyPolicy::BackgroundPreparse,
            None,
            None,
        );
        match outcome {
            Ok(ref o) => {
                lazy_refresh_queue.record_lazy_writes(&o.built_file_ids);
                lazy_refresh_queue.signal_background_writes();
                let result = json!({
                    "files_built": o.files_built,
                    "files_cached": o.files_cached,
                    "pending_job_ids": o.pending_job_ids,
                    "budget_exceeded": o.budget_exceeded,
                    "built_file_ids_count": o.built_file_ids.len(),
                });
                tm.complete_task(&tid, result);
            }
            Err(e) => {
                // Best-effort: signal background writes even on error.
                lazy_refresh_queue.signal_background_writes();
                tm.fail_task(&tid, &format!("Preparse failed: {e:#}"));
            }
        }
        // Note: graph refresh is not done here — preparse is best-effort.
        // The next sync graph-backed request will trigger maybe_refresh_graph.
    });

    task_id
}
