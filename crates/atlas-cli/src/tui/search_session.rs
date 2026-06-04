//! TUI SearchSession — thin wrapper that adds lazy structural fallback to
//! normal searches.  When a manifest-only search returns empty results, the
//! session triggers lazy structural extraction for the query, then re-searches.
//!
//! Analogous to the MCP's `SearchSession`, but decoupled from protocol concerns
//! and using the `Engine` facade for lazy structural access.
//!
//! ## Query parsing
//!
//! Queries are parsed for structured prefixes that match MCP search semantics:
//! - `lang:<ext>` — language filter (e.g. `lang:ts`, `lang:py`)
//! - `path:<dir>` — directory scope (e.g. `path:src`)
//! - `name:<term>` — symbol name filter
//! - Legacy `dir/: query` format — directory scope with slash separator
//!
//! Unknown prefixes and the remaining text become the freetext search term.

use std::path::Path;
use std::sync::Arc;

use atlas_engine::{
    Engine, GraphEngine, Language, SearchEngine, SearchOptions, SearchResult, Store, parse_query,
};

/// Parsed TUI search query with separated scope/filter and term.
#[derive(Debug, Clone)]
pub struct ParsedSearch {
    /// The actual search term (without language/scope/name prefixes).
    pub search_term: String,
    /// Language filter extracted from `lang:` prefix or extension alias.
    pub language: Option<Language>,
    /// Scope path filter extracted from `path:` prefix or legacy `dir/:` format.
    pub scope_path: Option<String>,
}

/// Thin search wrapper with lazy-structural fallback.
///
/// Constructed from the TUI `GraphSession`'s components.  Provides convenience
/// methods that compose the normal search path with lazy structural triggering
/// when results are empty (manifest-only index scenario).
pub struct SearchSession<'a> {
    engine: &'a Engine,
    #[allow(dead_code)]
    store: &'a Arc<Store>,
    #[allow(dead_code)]
    search: &'a SearchEngine,
    #[allow(dead_code)]
    graph: &'a Arc<GraphEngine>,
    #[allow(dead_code)]
    project_root: &'a Path,
}

impl<'a> SearchSession<'a> {
    /// Create a new search session from the TUI session's components.
    pub fn new(
        engine: &'a Engine,
        store: &'a Arc<Store>,
        search: &'a SearchEngine,
        graph: &'a Arc<GraphEngine>,
        project_root: &'a Path,
    ) -> Self {
        Self {
            engine,
            store,
            search,
            graph,
            project_root,
        }
    }

    /// Parse a raw TUI search query into a [`ParsedSearch`].
    ///
    /// Handles:
    /// - MCP-style structured prefixes: `lang:ts`, `path:src`, `name:auth`
    /// - Legacy `dir/: query` format for directory scoping
    /// - Plain freetext (substring or symbol name search)
    ///
    /// The resulting [`ParsedSearch`] separates the actual search term from
    /// scope/language filters so they can be passed independently to the
    /// search engine and lazy structural service.
    pub fn parse_query(raw_query: &str) -> ParsedSearch {
        // Try structured query parsing (lang:ts, path:src style)
        let engineered = parse_query(raw_query);

        let language = engineered.language;
        let structured_scope = engineered.path_filter;

        // Start with freetext or name_filter as the search term
        let mut search_term = engineered.freetext;
        if search_term.is_empty() {
            if let Some(ref name) = engineered.name_filter {
                search_term = name.clone();
            }
        }

        // Legacy "dir/: query" format: check for path-prefix slash syntax
        // (e.g. "src/: handler"). This fires when the raw query does not
        // match structured prefixes (lang:/path:/name:/kind:).
        let mut legacy_scope: Option<String> = None;
        let mut legacy_term: Option<String> = None;
        if let Some((prefix, rest)) = raw_query.split_once('/') {
            if prefix
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                if !rest.is_empty() && !prefix.contains(' ') {
                    legacy_scope = Some(format!("{}/", prefix));
                    legacy_term = Some(rest.trim().to_string());
                }
            }
        }

        // Decide scope: structured path_filter wins; legacy is fallback.
        let scope_path = structured_scope.or(legacy_scope);

        // Decide term: when structured freetext is identical to the raw query
        // (meaning no structured prefixes matched at all), prefer the legacy
        // term extracted from the slash split. Otherwise keep the structured
        // freetext (which already has prefixes stripped).
        let final_term = if search_term == raw_query {
            legacy_term.unwrap_or(search_term)
        } else if search_term.is_empty() {
            raw_query.to_string()
        } else {
            search_term
        };

        ParsedSearch {
            search_term: final_term,
            language,
            scope_path,
        }
    }

    /// Trigger lazy structural extraction for the given parsed search.
    ///
    /// Uses `Engine::lazy_structural()` to find candidate files via FTS5
    /// (and ripgrep fallback) for the *search term* (not raw query with
    /// prefixes), then builds their structural layer so subsequent searches
    /// can find symbols from those files.
    ///
    /// Returns `true` if any files were built or cached (i.e. structural
    /// work was done that may have changed the database).
    pub fn ensure_structural_for_search(&self, parsed: &ParsedSearch) -> anyhow::Result<bool> {
        tracing::info!(
            "TUI search empty for '{}', triggering lazy structural extraction",
            parsed.search_term
        );
        let ensured = self
            .engine
            .lazy_structural()
            .ensure_structural_for_symbol(&parsed.search_term)?;
        tracing::info!(
            "Lazy structural: {} built, {} cached, {} pending",
            ensured.files_built,
            ensured.files_cached,
            ensured.files_pending,
        );
        Ok(ensured.files_built > 0 || ensured.files_cached > 0)
    }

    /// Perform a search via the search engine using the parsed search info.
    ///
    /// Builds [`SearchOptions`] from the parsed language/scope_path filters
    /// and passes them to the search engine along with the extracted search term.
    pub fn do_search(
        search: &SearchEngine,
        parsed: &ParsedSearch,
        max_results: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut options = SearchOptions::new();
        if let Some(lang) = parsed.language {
            options = options.with_language(lang);
        }
        if let Some(ref path) = parsed.scope_path {
            options = options.with_file_path(path.clone());
        }
        search.search(&parsed.search_term, max_results, &options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::Store;

    /// Verify SearchSession can be constructed without panicking.
    #[test]
    fn search_session_constructs() {
        let store = Arc::new(Store::open_in_memory().expect("in-memory store"));
        store.init_schema().expect("schema init");

        let graph = Arc::new(GraphEngine::from_store(&store, 0.0_f32).expect("graph from store"));
        let search = SearchEngine::new(Arc::clone(&store), Arc::clone(&graph));

        let engine = Engine::from_store(Arc::clone(&store), None);
        let project_root = Path::new(".");

        let session = SearchSession::new(&engine, &store, &search, &graph, project_root);
        // Ensure structural on empty DB should return false (no candidates found).
        let parsed = SearchSession::parse_query("test");
        let triggered = session
            .ensure_structural_for_search(&parsed)
            .expect("ensure_structural_for_search succeeds");
        assert!(
            !triggered,
            "empty DB should have no candidates, so lazy should not trigger"
        );
    }

    // ── Query parsing tests ─────────────────────────────────────────────

    #[test]
    fn parse_query_freetext() {
        let parsed = SearchSession::parse_query("handler");
        assert_eq!(parsed.search_term, "handler");
        assert!(parsed.language.is_none());
        assert!(parsed.scope_path.is_none());
    }

    #[test]
    fn parse_query_lang_prefix() {
        let parsed = SearchSession::parse_query("lang:ts interface");
        assert_eq!(parsed.search_term, "interface");
        assert_eq!(parsed.language, Some(Language::TypeScript));
        assert!(parsed.scope_path.is_none());
    }

    #[test]
    fn parse_query_lang_cangjie() {
        let parsed = SearchSession::parse_query("lang:cj main");
        #[cfg(feature = "cangjie")]
        {
            assert_eq!(parsed.search_term, "main");
            assert_eq!(parsed.language, Some(Language::Cangjie));
        }
        #[cfg(not(feature = "cangjie"))]
        {
            // When cangjie isn't available, "cj" is not a recognized language.
            // The entire "lang:cj" token becomes freetext.
            assert_eq!(parsed.search_term, "lang:cj main");
            assert!(parsed.language.is_none());
        }
    }

    #[test]
    fn parse_query_path_prefix() {
        let parsed = SearchSession::parse_query("path:src handler");
        assert_eq!(parsed.search_term, "handler");
        assert_eq!(parsed.scope_path.as_deref(), Some("src"));
        assert!(parsed.language.is_none());
    }

    #[test]
    fn parse_query_legacy_slash_scope() {
        let parsed = SearchSession::parse_query("src/ handler");
        assert_eq!(parsed.search_term, "handler");
        assert_eq!(parsed.scope_path.as_deref(), Some("src/"));
        assert!(parsed.language.is_none());
    }

    #[test]
    fn parse_query_combined_prefixes() {
        let parsed = SearchSession::parse_query("lang:ts path:src name:auth authenticate");
        assert_eq!(parsed.search_term, "authenticate");
        assert_eq!(parsed.language, Some(Language::TypeScript));
        assert_eq!(parsed.scope_path.as_deref(), Some("src"));
    }
}
