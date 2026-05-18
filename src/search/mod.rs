//! Search engine: FTS5 full-text, fuzzy matching, multi-signal scoring.
//!
//! The `SearchEngine` combines:
//!   - FTS5 full-text index (from `symbols_fts` virtual table)
//!   - Graph degree signals (calls + references → centrality)
//!   - Fuzzy name matching (Levenshtein fallback)
//!   - Multi-signal relevance scoring (BM25 + degree + kind bonus)

pub mod fts;
pub mod fuzzy;
pub mod scoring;

use crate::db::Store;
use crate::graph::GraphEngine;
use crate::types::{FileId, Language, SymbolDef, SymbolKind};
use std::sync::Arc;

use self::scoring::SearchScore;

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
}

/// Unified search engine powered by FTS5 + graph signals.
pub struct SearchEngine {
    store: Arc<Store>,
    graph: Arc<GraphEngine>,
}

impl SearchEngine {
    pub fn new(store: Arc<Store>, graph: Arc<GraphEngine>) -> Self {
        Self { store, graph }
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Full-text + fuzzy search for symbols, ranked by multi-signal score.
    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let raw_results = self.store.search_symbols_with_limit(query, limit.max(20))?;
        if raw_results.is_empty() {
            return Ok(Vec::new());
        }

        let total_symbols = self.store.count_symbols()?;
        let matching_symbols = raw_results.len() as usize;

        // Compute global max degree for normalization
        let max_degree = raw_results
            .iter()
            .map(|s| self.graph.degree(&s.id))
            .max()
            .unwrap_or(1);

        let mut results: Vec<SearchResult> = Vec::with_capacity(raw_results.len());
        for sym in raw_results {
            let name_sim = compute_name_similarity(query, &sym.name);
            let path_match = sym.qualified_name.to_lowercase().contains(&query.to_lowercase());
            let degree = self.graph.degree(&sym.id);
            let idf = scoring::idf_weight(total_symbols, matching_symbols);

            let score = SearchScore::new(
                idf.clamp(0.0, 1.0),
                degree,
                max_degree,
                name_sim,
                sym.kind,
                path_match,
            );

            results.push(SearchResult {
                symbol: sym,
                score,
                matched_field: String::new(),
                snippet: None,
            });
        }

        // Sort by total score descending
        results.sort_by(|a, b| b.score.total.partial_cmp(&a.score.total).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results)
    }

    /// Search for symbols of a specific kind.
    pub fn search_by_kind(
        &self,
        query: &str,
        kind: SymbolKind,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut results = self.search(query, limit * 2)?;
        results.retain(|r| r.symbol.kind == kind);
        results.truncate(limit);
        Ok(results)
    }

    /// Search for symbols within a specific file.
    pub fn search_in_file(
        &self,
        query: &str,
        file_id: &FileId,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut results = self.search(query, limit * 2)?;
        results.retain(|r| r.symbol.file_id == *file_id);
        results.truncate(limit);
        Ok(results)
    }

    /// Fuzzy name search — useful for typo-tolerant symbol lookup.
    pub fn fuzzy_search(&self, name: &str, language: Option<Language>, limit: usize) -> anyhow::Result<Vec<(SymbolDef, f64)>> {
        // Broader FTS5 search to get candidates
        // Use first 3 chars as FTS5 prefix for candidate pool, then score by full name
        let prefix: String = name.chars().take(3).collect();
        let candidates = self.store.search_symbols_with_limit(&prefix, limit.max(50))?;
        let mut scored: Vec<(SymbolDef, f64)> = candidates
            .into_iter()
            .filter(|s| language.map_or(true, |l| s.language == l))
            .map(|s| {
                let sim = compute_name_similarity(name, &s.name);
                (s, sim)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

/// Compute name similarity between query and candidate name.
/// Exact → 1.0; case-insensitive → 0.9; Levenshtein ratio → 0.0..0.7.
fn compute_name_similarity(query: &str, name: &str) -> f64 {
    if query == name {
        return 1.0;
    }
    if query.eq_ignore_ascii_case(name) {
        return 0.9;
    }
    let dist = crate::search::fuzzy::levenshtein(&query.to_lowercase(), &name.to_lowercase());
    let max_len = query.len().max(name.len()).max(1);
    let ratio = 1.0 - (dist as f64 / max_len as f64);
    // Scale fuzzy ratio: 0.7 is max for non-exact fuzzy matches
    ratio * 0.7
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Store;
    use crate::graph::GraphEngine;
    use crate::types::{FileInfo, Language, ParseStatus, SymbolDef};
    use std::sync::Arc;

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
        let fid = crate::types::FileId::generate("test.ts");
        store.upsert_file(&FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: Language::TypeScript,
            content_hash: "abc".into(),
            status: ParseStatus::Success,
        }).unwrap();

        let syms: Vec<SymbolDef> = vec![
            mk_sym(fid, "UserManager", "UserManager", SymbolKind::Class),
            mk_sym(fid, "createUser", "UserManager.createUser", SymbolKind::Method),
            mk_sym(fid, "deleteUser", "UserManager.deleteUser", SymbolKind::Method),
            mk_sym(fid, "UserRouter", "UserRouter", SymbolKind::Class),
            mk_sym(fid, "findUser", "UserRouter.findUser", SymbolKind::Method),
        ];
        store.insert_symbols(&syms).unwrap();
    }

    fn mk_sym(fid: FileId, name: &str, qname: &str, kind: SymbolKind) -> SymbolDef {
        let sid = crate::types::SymbolId::generate(&fid, "ts", qname, kind.as_str(), None);
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
        }
    }

    #[test]
    fn test_search_basic() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        let results = engine.search("User", 10).unwrap();
        assert!(!results.is_empty());
        // "UserManager" and "UserRouter" should rank high
        let top_name = &results[0].symbol.name;
        assert!(top_name.contains("User"));
    }

    #[test]
    fn test_search_empty() {
        let store = test_store();
        let engine = test_engine(store);
        let results = engine.search("", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_kind() {
        let store = test_store();
        seed_symbols(&store);
        let engine = test_engine(store);

        let results = engine.search_by_kind("User", SymbolKind::Method, 10).unwrap();
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
}
