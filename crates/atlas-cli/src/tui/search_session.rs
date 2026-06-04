//! TUI SearchSession — thin wrapper that adds lazy structural fallback to
//! normal searches.  When a manifest-only search returns empty results, the
//! session triggers lazy structural extraction for the query, then re-searches.
//!
//! Analogous to the MCP's `SearchSession`, but decoupled from protocol concerns
//! and using the `Engine` facade for lazy structural access.

use std::path::Path;
use std::sync::Arc;

use atlas_engine::{Engine, GraphEngine, SearchEngine, SearchOptions, SearchResult, Store};

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

    /// Trigger lazy structural extraction for the given query.
    ///
    /// Uses `Engine::lazy_structural()` to find candidate files via FTS5
    /// and build their structural layer so subsequent searches can find
    /// symbols from those files.
    ///
    /// Returns `true` if any files were built or cached (i.e. structural
    /// work was done that may have changed the database).
    pub fn ensure_structural_for_search(&self, query: &str) -> anyhow::Result<bool> {
        tracing::info!(
            "TUI search empty for '{query}', triggering lazy structural extraction"
        );
        let ensured = self
            .engine
            .lazy_structural()
            .ensure_structural_for_symbol(query)?;
        tracing::info!(
            "Lazy structural: {} built, {} cached, {} pending",
            ensured.files_built,
            ensured.files_cached,
            ensured.files_pending,
        );
        Ok(ensured.files_built > 0 || ensured.files_cached > 0)
    }

    /// Perform a search via the search engine with optional scope path.
    pub fn do_search(
        search: &SearchEngine,
        query: &str,
        scope_path: Option<&str>,
        max_results: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        if let Some(path_pattern) = scope_path {
            let options = SearchOptions::new().with_file_path(path_pattern.to_string());
            search.search(query, max_results, &options)
        } else {
            search.search_simple(query, max_results)
        }
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

        let graph = Arc::new(
            GraphEngine::from_store(&store, 0.0_f32).expect("graph from store"),
        );
        let search = SearchEngine::new(Arc::clone(&store), Arc::clone(&graph));

        let engine = Engine::from_store(Arc::clone(&store), None);
        let project_root = Path::new(".");

        let session = SearchSession::new(&engine, &store, &search, &graph, project_root);
        // Ensure structural on empty DB should return false (no candidates found).
        let triggered = session
            .ensure_structural_for_search("test")
            .expect("ensure_structural_for_search succeeds");
        assert!(
            !triggered,
            "empty DB should have no candidates, so lazy should not trigger"
        );
    }
}
