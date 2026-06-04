//! Search engine: FTS5 full-text, LIKE fallback, Levenshtein fuzzy, multi-signal scoring.
//!
//! The `SearchEngine` combines a three-tier fallback search strategy:
//!   1. FTS5 full-text index (BM25, prefix matching via `symbols_fts`)
//!   2. LIKE substring search (when FTS5 returns nothing)
//!   3. Levenshtein edit distance fuzzy match (final fallback for typos)
//!
//! Plus multi-signal scoring: BM25/IDF + graph degree + name similarity + kind bonus + path relevance

pub mod fts;
pub mod fuzzy;
pub mod query_parser;
pub mod scoring;

use db::Store;
use graph::GraphEngine;
use std::sync::{Arc, RwLock};
use types::{FileId, Language, SymbolDef, SymbolKind};

use self::scoring::SearchScore;

/// Search filter options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Filter results to this language only.
    pub language: Option<Language>,
    /// Filter results where file path contains this substring.
    pub file_path_pattern: Option<String>,
    /// Filter results to this symbol kind only.
    pub kind_filter: Option<SymbolKind>,
    /// Minimum confidence threshold for fuzzy matches (0.0..1.0).
    pub min_confidence: Option<f64>,
}

impl SearchOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_language(mut self, lang: Language) -> Self {
        self.language = Some(lang);
        self
    }

    pub fn with_file_path(mut self, path: impl Into<String>) -> Self {
        self.file_path_pattern = Some(path.into());
        self
    }

    pub fn with_kind(mut self, kind: SymbolKind) -> Self {
        self.kind_filter = Some(kind);
        self
    }

    pub fn with_confidence(mut self, c: f64) -> Self {
        self.min_confidence = Some(c);
        self
    }
}

/// A single search result with cumulative score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The matched symbol.
    pub symbol: SymbolDef,
    /// Multi-signal relevance score.
    pub score: SearchScore,
    /// Which field matched — empty string means general match.
    pub matched_field: String,
    /// Brief snippet from the symbol context.
    pub snippet: Option<String>,
    /// Resolved file path (human-readable, e.g. "src/main.rs").
    pub file_path: Option<String>,
}

/// Unified search engine powered by FTS5 + graph signals.
pub struct SearchEngine {
    store: Arc<Store>,
    graph: RwLock<Arc<GraphEngine>>,
}

impl SearchEngine {
    pub fn new(store: Arc<Store>, graph: Arc<GraphEngine>) -> Self {
        Self {
            store,
            graph: RwLock::new(graph),
        }
    }

    /// Replace the internal graph snapshot with a fresh one.
    pub fn refresh_graph(&self, graph: Arc<GraphEngine>) {
        *self.graph.write().unwrap_or_else(|e| e.into_inner()) = graph;
    }

    /// Acquire a read-lock on the graph, recovering from poison.
    fn graph(&self) -> std::sync::RwLockReadGuard<'_, Arc<GraphEngine>> {
        self.graph.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Return a clone of the current graph snapshot.
    pub fn graph_snapshot(&self) -> Arc<GraphEngine> {
        Arc::clone(&*self.graph())
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Full-text + LIKE fallback + fuzzy search, with optional filters.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let total_symbols = self.store.count_symbols()?;

        // Stage 1: FTS5 (kind filter pushed to SQL for correct fallback behavior)
        let mut raw_results = self.store.search_symbols_with_limit(
            query,
            limit.max(20),
            options.kind_filter.as_ref(),
        )?;
        let mut fts_had_results = !raw_results.is_empty();

        // Stage 2: LIKE fallback if FTS5 returns nothing
        let mut from_like = if raw_results.is_empty() && query.len() >= 2 {
            raw_results = self.store.search_symbols_by_name_like(
                query,
                options.language.as_ref(),
                limit.max(20),
                options.kind_filter.as_ref(),
            )?;
            true
        } else {
            false
        };

        // Stage 3: Fuzzy Levenshtein fallback if still nothing
        let from_fuzzy = if raw_results.is_empty() && query.len() >= 2 {
            // Load all symbols and compute Levenshtein distance to find close matches
            let fuzzy_start = std::time::Instant::now();
            let all_symbols = self.store.get_all_symbols()?;
            tracing::debug!(
                target: "atlas::search",
                "fuzzy fallback triggered: {} symbols loaded in {:?}, query='{}'",
                all_symbols.len(),
                fuzzy_start.elapsed(),
                query
            );
            let query_lower = query.to_lowercase();
            let query_norm_snake = to_snake_case(&normalize_name_for_search(query));
            let max_dist = (query.len() as f64 * 0.4).ceil() as usize; // allow ~40% edit distance
            let mut candidates: Vec<(SymbolDef, usize)> = Vec::new();
            for sym in &all_symbols {
                // Apply kind filter at scan level for consistent behavior
                if let Some(kind) = options.kind_filter {
                    if sym.kind != kind {
                        continue;
                    }
                }
                let name_lower = sym.name.to_lowercase();
                let name_snake = to_snake_case(&normalize_name_for_search(&sym.name));
                // Check both original and snake_case normalized forms
                let dist1 = fuzzy::levenshtein(&query_lower, &name_lower);
                let dist2 = fuzzy::levenshtein(&query_norm_snake, &name_snake);
                let min_dist = dist1.min(dist2);
                if min_dist <= max_dist {
                    candidates.push((sym.clone(), min_dist));
                }
            }
            // Sort by distance (closest first), take top limit
            candidates.sort_by_key(|c| c.1);
            raw_results = candidates
                .into_iter()
                .take(limit.max(50))
                .map(|(s, _)| s)
                .collect();
            if let Some(ref lang) = options.language {
                raw_results.retain(|s| s.language == *lang);
            }
            true
        } else {
            false
        };

        if raw_results.is_empty() {
            return Ok(Vec::new());
        }

        // Normalize query for camelCase/snake_case matching
        let query_norm = normalize_name_for_search(query);
        let weights = scoring::ScoreWeights::default();

        // Score → filter → (retry with LIKE if filters emptied FTS results)
        let mut results: Vec<SearchResult> = loop {
            let matching_symbols = raw_results.len();
            let graph = self.graph();
            let max_degree = raw_results
                .iter()
                .map(|s| graph.degree(&s.id))
                .max()
                .unwrap_or(1);

            let mut results = Vec::with_capacity(raw_results.len());
            for sym in &raw_results {
                let name_sim = compute_name_similarity(query, &sym.name, &query_norm);
                let qualified_match = sym
                    .qualified_name
                    .to_lowercase()
                    .contains(&query.to_lowercase());
                let degree = graph.degree(&sym.id);
                let idf = scoring::idf_weight(total_symbols, matching_symbols);

                // Resolve FileId → human-readable path
                let file_path = self
                    .store
                    .get_file(&sym.file_id)
                    .ok()
                    .flatten()
                    .map(|info| info.path);

                let score = SearchScore::new(
                    idf.clamp(0.0, 1.0),
                    degree,
                    max_degree,
                    name_sim,
                    qualified_match,
                    sym.kind,
                    file_path.as_deref(),
                    &weights,
                );

                // Determine matched field for display
                let matched_field = if from_like {
                    "name".to_string()
                } else if from_fuzzy {
                    "fuzzy".to_string()
                } else {
                    String::new()
                };

                results.push(SearchResult {
                    symbol: sym.clone(),
                    score,
                    matched_field,
                    snippet: None,
                    file_path: file_path.clone(),
                });
            }

            // Apply post-filters BEFORE truncation (so all results, not just top N,
            // are considered for filter matching).
            if let Some(ref path_pat) = options.file_path_pattern {
                let pat = path_pat.to_lowercase();
                results.retain(|r| {
                    r.file_path
                        .as_ref()
                        .map(|p| p.to_lowercase().contains(&pat))
                        .unwrap_or(false)
                });
            }
            if let Some(ref lang_filter) = options.language {
                results.retain(|r| r.symbol.language == *lang_filter);
            }
            if let Some(min_c) = options.min_confidence {
                results.retain(|r| r.score.total >= min_c);
            }

            // LIKE fallback: if FTS5 originally returned data but post-filters
            // eliminated everything (e.g. all top-N were TypeScript but user
            // filtered for lang:python), retry with LIKE substring search.
            if results.is_empty() && fts_had_results && !from_like && query.len() >= 2 {
                raw_results = self.store.search_symbols_by_name_like(
                    query,
                    options.language.as_ref(),
                    limit.max(20),
                    options.kind_filter.as_ref(),
                )?;
                if raw_results.is_empty() {
                    break results;
                }
                from_like = true;
                fts_had_results = false;
                continue;
            }
            break results;
        };

        // Sort by total score descending, then truncate to limit
        results.sort_by(|a, b| {
            b.score
                .total
                .partial_cmp(&a.score.total)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        Ok(results)
    }

    /// Convenience: search without options (backward-compatible).
    pub fn search_simple(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        self.search(query, limit, &SearchOptions::default())
    }

    /// Search for symbols of a specific kind.
    pub fn search_by_kind(
        &self,
        query: &str,
        kind: SymbolKind,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        self.search(query, limit, &SearchOptions::new().with_kind(kind))
    }

    /// Search for symbols within a specific file.
    pub fn search_in_file(
        &self,
        query: &str,
        file_id: &FileId,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let file_path = self
            .store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|info| info.path)
            .unwrap_or_default();
        self.search(
            query,
            limit,
            &SearchOptions::new().with_file_path(&file_path),
        )
    }

    /// Fuzzy name search — useful for typo-tolerant symbol lookup.
    pub fn fuzzy_search(
        &self,
        name: &str,
        language: Option<Language>,
        limit: usize,
    ) -> anyhow::Result<Vec<(SymbolDef, f64)>> {
        let options = SearchOptions {
            language,
            ..Default::default()
        };
        let results = self.search(name, limit, &options)?;
        Ok(results
            .into_iter()
            .map(|r| (r.symbol, r.score.name_score))
            .collect())
    }
}

// ------------------------------------------------------------------
// CamelCase / snake_case normalization
// ------------------------------------------------------------------

/// Split a camelCase or PascalCase name into words.
fn split_camel_case(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut prev_upper = false;
    for ch in s.chars() {
        if ch == '_' {
            if !current.is_empty() {
                words.push(current.to_lowercase());
                current.clear();
            }
            prev_upper = false;
        } else if ch.is_uppercase() {
            if !current.is_empty() && !prev_upper {
                words.push(current.to_lowercase());
                current.clear();
            }
            current.push(ch);
            prev_upper = true;
        } else {
            if prev_upper && current.len() > 1 {
                // Handle "XMLParser" → "XML", "Parser"
                let last = current.pop().unwrap();
                words.push(current.to_lowercase());
                current.clear();
                current.push(last);
            }
            current.push(ch);
            prev_upper = false;
        }
    }
    if !current.is_empty() {
        words.push(current.to_lowercase());
    }
    words
}

/// Normalize a name for search matching: convert to snake_case word list.
fn normalize_name_for_search(name: &str) -> Vec<String> {
    split_camel_case(name)
}

/// Join normalized words back into a snake_case string.
fn to_snake_case(words: &[String]) -> String {
    words.join("_")
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

/// Compute name similarity between query and candidate name.
///
/// Strategies (in order):
///   1. Exact match → 1.0
///   2. Case-insensitive match → 0.9
///   3. CamelCase/snake_case normalization match → 0.85
///   4. Levenshtein on normalized forms → 0.0..0.7
fn compute_name_similarity(query: &str, name: &str, query_norm: &[String]) -> f64 {
    if query == name {
        return 1.0;
    }
    if query.eq_ignore_ascii_case(name) {
        return 0.9;
    }

    // Prefix match: name starts with query (e.g., "Browser" → "BrowserSpider")
    // This is a strong signal that the user is looking for this symbol.
    let query_lower = query.to_lowercase();
    let name_lower = name.to_lowercase();
    if name_lower.starts_with(&query_lower) && query.len() >= 2 {
        return 0.92;
    }

    // CamelCase/snake_case normalization
    let name_norm = normalize_name_for_search(name);
    let query_snake = to_snake_case(query_norm);
    let name_snake = to_snake_case(&name_norm);
    if query_snake == name_snake {
        return 0.85;
    }
    if query_snake.eq_ignore_ascii_case(&name_snake) {
        return 0.8;
    }

    // Word-level matching: if any normalized word matches, boost slightly
    let word_match = query_norm
        .iter()
        .any(|w| !w.is_empty() && name_norm.iter().any(|nw| nw == w));
    if word_match {
        // Partial word match — scale by how many words overlap
        let overlap = query_norm.iter().filter(|w| name_norm.contains(w)).count();
        let total = query_norm.len().max(name_norm.len()).max(1);
        return 0.5 + (overlap as f64 / total as f64) * 0.25;
    }

    // Levenshtein fallback on snake_case forms
    let dist = crate::fuzzy::levenshtein(&query_snake, &name_snake);
    let max_len = query_snake.len().max(name_snake.len()).max(1);
    let ratio = 1.0 - (dist as f64 / max_len as f64);
    ratio * 0.7
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use graph::GraphEngine;
    use std::sync::Arc;
    use types::{FileInfo, Language, ParseStatus, SymbolDef};

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    fn test_engine(store: Arc<Store>) -> SearchEngine {
        let graph = Arc::new(GraphEngine::from_store(&store, 0.0_f32).unwrap());
        SearchEngine::new(store, graph)
    }

    fn seed_symbols(store: &Store) {
        let fid = types::FileId::generate("test.ts");
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: "test.ts".into(),
                language: Language::TypeScript,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let syms: Vec<SymbolDef> = vec![
            mk_sym(fid, "UserManager", "UserManager", SymbolKind::Class),
            mk_sym(
                fid,
                "createUser",
                "UserManager.createUser",
                SymbolKind::Method,
            ),
            mk_sym(
                fid,
                "deleteUser",
                "UserManager.deleteUser",
                SymbolKind::Method,
            ),
            mk_sym(fid, "UserRouter", "UserRouter", SymbolKind::Class),
            mk_sym(fid, "findUser", "UserRouter.findUser", SymbolKind::Method),
            mk_sym(
                fid,
                "get_user_name",
                "UserManager.get_user_name",
                SymbolKind::Method,
            ),
        ];
        store.insert_symbols(&syms).unwrap();
    }

    fn mk_sym(fid: FileId, name: &str, qname: &str, kind: SymbolKind) -> SymbolDef {
        let sid = types::SymbolId::generate(&fid, "ts", qname, kind.as_str(), None);
        SymbolDef {
            id: sid,
            kind,
            name: name.into(),
            qualified_name: qname.into(),
            symbol_path: vec![],
            file_id: fid,
            language: Language::TypeScript,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    #[test]
    fn test_search_basic() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        let results = engine.search_simple("User", 10).unwrap();
        assert!(!results.is_empty());
        let top_name = &results[0].symbol.name;
        assert!(top_name.contains("User"));
    }

    #[test]
    fn test_search_empty() {
        let store = test_store();
        let engine = test_engine(store);
        let results = engine.search_simple("", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_kind() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        let results = engine
            .search_by_kind("User", SymbolKind::Method, 10)
            .unwrap();
        for r in &results {
            assert_eq!(r.symbol.kind, SymbolKind::Method);
        }
    }

    #[test]
    fn test_fuzzy_search() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        let results = engine.fuzzy_search("UserMnger", None, 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_camel_case_split() {
        assert_eq!(split_camel_case("getUser"), vec!["get", "user"]);
        assert_eq!(split_camel_case("UserManager"), vec!["user", "manager"]);
        assert_eq!(
            split_camel_case("get_user_name"),
            vec!["get", "user", "name"]
        );
        assert_eq!(split_camel_case("XMLParser"), vec!["xml", "parser"]);
        assert_eq!(split_camel_case("simple"), vec!["simple"]);
    }

    #[test]
    fn test_normalize_name_for_search() {
        let norm = normalize_name_for_search("getUser");
        assert_eq!(to_snake_case(&norm), "get_user");

        let norm = normalize_name_for_search("get_user_name");
        assert_eq!(to_snake_case(&norm), "get_user_name");

        let norm = normalize_name_for_search("UserManager");
        assert_eq!(to_snake_case(&norm), "user_manager");
    }

    #[test]
    fn test_camel_case_similarity() {
        // "getUser" should match "get_user"
        let query_norm = normalize_name_for_search("getUser");
        let sim = compute_name_similarity("getUser", "get_user", &query_norm);
        assert!(sim > 0.8, "Expected high similarity, got {sim}");

        // "UserManager" should match "user_manager"
        let query_norm = normalize_name_for_search("UserManager");
        let sim = compute_name_similarity("UserManager", "user_manager", &query_norm);
        assert!(sim > 0.8, "Expected high similarity, got {sim}");
    }

    #[test]
    fn test_like_fallback_search() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        // "anager" won't match FTS5 prefix, but LIKE should find "UserManager"
        let results = engine.search_simple("anager", 10).unwrap();
        assert!(
            !results.is_empty(),
            "LIKE fallback should find 'UserManager'"
        );
    }

    #[test]
    fn test_language_filter() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        let results = engine
            .search(
                "User",
                10,
                &SearchOptions::new().with_language(Language::TypeScript),
            )
            .unwrap();
        for r in &results {
            assert_eq!(r.symbol.language, Language::TypeScript);
        }
    }

    // ── helpers for multi-language / multi-file tests ──

    fn mk_sym_lang(
        fid: FileId,
        name: &str,
        qname: &str,
        kind: SymbolKind,
        lang: Language,
    ) -> SymbolDef {
        let sid = types::SymbolId::generate(&fid, "ts", qname, kind.as_str(), None);
        SymbolDef {
            id: sid,
            kind,
            name: name.into(),
            qualified_name: qname.into(),
            symbol_path: vec![],
            file_id: fid,
            language: lang,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    fn seed_mixed_language_symbols(store: &Store) {
        let ts_fid = types::FileId::generate("test.ts");
        store
            .upsert_file(&FileInfo {
                file_id: ts_fid,
                path: "test.ts".into(),
                language: Language::TypeScript,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let py_fid = types::FileId::generate("test.py");
        store
            .upsert_file(&FileInfo {
                file_id: py_fid,
                path: "test.py".into(),
                language: Language::Python,
                content_hash: "def".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let syms = vec![
            mk_sym_lang(
                ts_fid,
                "UserManager",
                "UserManager",
                SymbolKind::Class,
                Language::TypeScript,
            ),
            mk_sym_lang(
                ts_fid,
                "UserValidator",
                "UserValidator",
                SymbolKind::Class,
                Language::TypeScript,
            ),
            mk_sym_lang(
                py_fid,
                "UserConfig",
                "UserConfig",
                SymbolKind::Class,
                Language::Python,
            ),
        ];
        store.insert_symbols(&syms).unwrap();
    }

    fn seed_multi_file_symbols(store: &Store) {
        let fid_a = types::FileId::generate("a.ts");
        store
            .upsert_file(&FileInfo {
                file_id: fid_a,
                path: "src/a.ts".into(),
                language: Language::TypeScript,
                content_hash: "aaa".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let fid_b = types::FileId::generate("b.ts");
        store
            .upsert_file(&FileInfo {
                file_id: fid_b,
                path: "src/b.ts".into(),
                language: Language::TypeScript,
                content_hash: "bbb".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let syms = vec![
            mk_sym(fid_a, "LoggerA", "LoggerA", SymbolKind::Class),
            mk_sym(fid_b, "LoggerB", "LoggerB", SymbolKind::Class),
        ];
        store.insert_symbols(&syms).unwrap();
    }

    // ── new tests ──

    #[test]
    fn test_lang_filter_with_mixed_languages() {
        // When FTS5 returns results from multiple languages, the language
        // post-filter should be applied to ALL results before truncation,
        // not just the top N.
        let store = test_store();
        seed_mixed_language_symbols(&store);
        let engine = test_engine(store);

        let results = engine
            .search(
                "User",
                10,
                &SearchOptions::new().with_language(Language::Python),
            )
            .unwrap();
        assert!(
            !results.is_empty(),
            "language filter should find Python results"
        );
        for r in &results {
            assert_eq!(
                r.symbol.language,
                Language::Python,
                "all results must be Python, got {:?}",
                r.symbol.language
            );
        }
    }

    #[test]
    fn test_path_filter_before_truncate() {
        // Path filter must be applied to ALL scored results before
        // truncation, so symbols in "b.ts" are not hiding behind the
        // truncation barrier.
        let store = test_store();
        seed_multi_file_symbols(&store);
        let engine = test_engine(store);

        let results = engine
            .search(
                "Logger",
                10,
                &SearchOptions::new().with_file_path("a.ts"),
            )
            .unwrap();
        assert!(
            !results.is_empty(),
            "path filter should find matches in a.ts"
        );
        for r in &results {
            let path = r.file_path.as_deref().unwrap_or("");
            assert!(
                path.contains("a.ts"),
                "all results must be from a.ts, got: {path}"
            );
        }
    }

    #[test]
    fn test_like_fallback_when_fts_post_filter_empty() {
        // Scenario: FTS5 returns TypeScript results for query "User",
        // but user filters for lang:python. After post-filter, results
        // are empty. The LIKE fallback should kick in and find the
        // Python symbol ("PythonUserHelper") that FTS5 prefix matching
        // missed (because "User*" does not match a token starting with
        // "Python").
        let store = test_store();
        // Seed TS symbols that FTS will match, and a Python symbol
        // that only LIKE can find.
        let ts_fid = types::FileId::generate("test.ts");
        store
            .upsert_file(&FileInfo {
                file_id: ts_fid,
                path: "test.ts".into(),
                language: Language::TypeScript,
                content_hash: "ts1".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let py_fid = types::FileId::generate("test.py");
        store
            .upsert_file(&FileInfo {
                file_id: py_fid,
                path: "test.py".into(),
                language: Language::Python,
                content_hash: "py1".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let syms = vec![
            // FTS "User*" matches these
            mk_sym_lang(
                ts_fid,
                "UserManager",
                "UserManager",
                SymbolKind::Class,
                Language::TypeScript,
            ),
            mk_sym_lang(
                ts_fid,
                "UserService",
                "UserService",
                SymbolKind::Class,
                Language::TypeScript,
            ),
            // FTS "User*" does NOT match "PythonUserHelper"
            // (token starts with "Python", not "User"),
            // but LIKE "%User%" will find it.
            mk_sym_lang(
                py_fid,
                "PythonUserHelper",
                "PythonUserHelper",
                SymbolKind::Class,
                Language::Python,
            ),
        ];
        store.insert_symbols(&syms).unwrap();

        let engine = test_engine(store);
        let results = engine
            .search(
                "User",
                10,
                &SearchOptions::new().with_language(Language::Python),
            )
            .unwrap();
        assert!(
            !results.is_empty(),
            "LIKE fallback should find 'PythonUserHelper' after FTS-post-filter emptied results"
        );
        // Verify the result came from LIKE, not FTS
        assert_eq!(
            results[0].matched_field, "name",
            "result should come from LIKE fallback, got matched_field='{}'",
            results[0].matched_field
        );
        assert_eq!(
            results[0].symbol.language,
            Language::Python,
            "result should be a Python symbol"
        );
        assert_eq!(results[0].symbol.name, "PythonUserHelper");
    }
}
