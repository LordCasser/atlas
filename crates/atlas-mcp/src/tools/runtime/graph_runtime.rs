use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::{ContextBuilder, SearchEngine, SourceExtractor, Store};

use crate::tools::graph_state::GraphState;
use crate::tools::lazy_refresh::LazyRefreshQueue;

use super::graph_provider::GraphProvider;

// ── Precision types ─────────────────────────────────────────────────────

/// The mode of graph edge provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphMode {
    /// Full index: all edges are canonical, high-confidence.
    FullCanonical,
    /// Focus/closure mode: edges may be partial, lower-confidence.
    /// The graph may contain both canonical edges and closure-based edges.
    FocusPartial,
}

/// Precision information about the current graph snapshot.
#[derive(Debug, Clone)]
pub struct GraphPrecision {
    /// The graph mode (canonical vs partial).
    pub mode: GraphMode,
    /// Whether the graph has been initialized at all.
    pub initialized: bool,
    /// Total edge count in the current snapshot (approximate).
    pub edge_count: usize,
    /// Approximate symbol count in the current snapshot.
    /// Reserved for future MCP response enrichment (e.g. `symbol_count` in status output).
    #[allow(dead_code)]
    pub symbol_count: usize,
}

// ── GraphRuntime ────────────────────────────────────────────────────────

/// Manages the in-memory call graph snapshot lifecycle.
///
/// Provides lazy initialization and incremental refresh of the
/// SearchEngine and ContextBuilder backed by GraphState.
pub struct GraphRuntime {
    pub state: GraphState,
    pub store: Arc<Store>,
    pub source_extractor: SourceExtractor,
    pub project_root: PathBuf,
    /// Provenance mode of graph edges (detected on first init).
    pub mode: GraphMode,
    /// Cached store index mode at graph init time.
    cached_index_mode: Option<String>,
}

impl GraphRuntime {
    pub fn new(
        store: Arc<Store>,
        source_extractor: SourceExtractor,
        project_root: PathBuf,
    ) -> Self {
        let last_graph_signature = store.index_signature().unwrap_or_default();
        Self {
            state: GraphState {
                search: None,
                context: None,
                graph_initialized: false,
                last_graph_signature,
                pending_graph_rebuild: Arc::new(std::sync::Mutex::new(None)),
            },
            store,
            source_extractor,
            project_root,
            mode: GraphMode::FocusPartial,
            cached_index_mode: None,
        }
    }

    /// Ensure the graph is initialized (lazy init on first query).
    /// Detects and caches the graph provenance mode on first init.
    /// Returns &SearchEngine or an error.
    pub fn ensure_initialized(&mut self) -> anyhow::Result<&SearchEngine> {
        let was_initialized = self.state.graph_initialized;
        self.state
            .ensure_initialized(&self.store, &self.source_extractor, &self.project_root)?;
        if !was_initialized {
            let store = self.store.clone();
            self.detect_and_set_mode(&store);
        }
        self.state.search_engine().map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Access the context builder (requires initialization).
    pub fn context_builder(&self) -> anyhow::Result<&ContextBuilder> {
        self.state
            .context_builder()
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Access the search engine (requires initialization).
    pub fn search_engine(&self) -> anyhow::Result<&SearchEngine> {
        self.state
            .search_engine()
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Try to apply a background-built graph or spawn a rebuild.
    #[allow(dead_code)]
    pub fn maybe_refresh(
        &mut self,
        lazy_refresh_queue: Arc<LazyRefreshQueue>,
    ) -> anyhow::Result<()> {
        self.state
            .try_apply_or_spawn_rebuild(self.store.clone(), lazy_refresh_queue);
        Ok(())
    }

    /// Refresh the graph snapshot after lazy extraction for specific files.
    #[allow(dead_code)]
    pub fn refresh_for_files(&mut self, file_ids: &[atlas_engine::FileId]) -> anyhow::Result<()> {
        self.state.refresh_graph_for_files(&self.store, file_ids)
    }

    /// Detect the index precision mode from the store and cache it.
    /// Uses the same rich-index detection as FocusRuntime.
    pub fn detect_and_set_mode(&mut self, store: &Store) {
        let index_mode = store.read_index_mode().unwrap_or_default();
        self.cached_index_mode = Some(index_mode.clone());
        self.mode = if atlas_engine::is_rich_index_mode(&index_mode) {
            GraphMode::FullCanonical
        } else {
            GraphMode::FocusPartial
        };
    }

    /// Return precision metadata about the current graph.
    pub fn precision_info(&self) -> GraphPrecision {
        GraphPrecision {
            mode: self.mode,
            initialized: self.state.graph_initialized,
            edge_count: self.state.edge_count(),
            symbol_count: self.state.symbol_count(),
        }
    }

    /// Returns the underlying graph backend implementing [`GraphProvider`].
    pub(crate) fn provider(&self) -> &dyn GraphProvider {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::Store;
    use std::sync::Arc;

    fn create_test_graph_runtime() -> GraphRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let source_extractor = SourceExtractor::new(store.clone(), PathBuf::from("."));
        GraphRuntime::new(store, source_extractor, PathBuf::from("."))
    }

    #[test]
    fn default_mode_is_focus_partial() {
        let gr = create_test_graph_runtime();
        assert_eq!(gr.mode, GraphMode::FocusPartial);
    }

    #[test]
    fn precision_info_reflects_mode() {
        let gr = create_test_graph_runtime();
        let info = gr.precision_info();
        assert_eq!(info.mode, GraphMode::FocusPartial);
        assert!(!info.initialized);
        assert_eq!(info.edge_count, 0);
    }

    #[test]
    fn detect_and_set_mode_respects_store() {
        let mut gr = create_test_graph_runtime();
        // Clone store to avoid simultaneous mutable+immutable borrow.
        let store = gr.store.clone();
        gr.detect_and_set_mode(&store);
        assert_eq!(gr.mode, GraphMode::FocusPartial);
    }

    #[test]
    fn precision_info_full_canonical() {
        let mut gr = create_test_graph_runtime();
        gr.mode = GraphMode::FullCanonical;
        let info = gr.precision_info();
        assert_eq!(info.mode, GraphMode::FullCanonical);
    }

    #[test]
    fn ensure_initialized_sets_up_search_engine() {
        let mut gr = create_test_graph_runtime();
        let result = gr.ensure_initialized();
        assert!(result.is_ok(), "ensure_initialized should succeed");
        let se_result = gr.search_engine();
        assert!(se_result.is_ok(), "search_engine should be accessible after init");
    }

    #[test]
    fn precision_info_includes_symbol_count() {
        let gr = create_test_graph_runtime();
        let info = gr.precision_info();
        // symbol_count field is reserved for future MCP response enrichment;
        // verify it is initialized and accessible.
        assert_eq!(info.symbol_count, 0);
    }

    #[test]
    fn maybe_refresh_does_not_panic_on_uninitialized_graph() {
        let mut gr = create_test_graph_runtime();
        let lrq = LazyRefreshQueue::new();
        let result = gr.maybe_refresh(lrq);
        assert!(result.is_ok());
    }

    #[test]
    fn refresh_for_files_handles_empty_list() {
        let mut gr = create_test_graph_runtime();
        let result = gr.refresh_for_files(&[]);
        assert!(result.is_ok());
    }

    #[test]
    fn provider_trait_contract_holds() {
        let mut gr = create_test_graph_runtime();
        {
            let p = gr.provider();
            assert!(!p.is_initialized());
            assert!(p.search_engine().is_none());
            assert!(p.context_builder().is_none());
            assert_eq!(p.node_count(), 0);
            assert_eq!(p.edge_count(), 0);
        }

        gr.ensure_initialized().unwrap();
        let p = gr.provider();
        assert!(p.is_initialized());
        // Using an empty store yields zero nodes/edges — this is expected.
    }
}
