//! Graph runtime — in-memory call-graph snapshot lifecycle.
//!
//! # Responsibilities
//! - Lazy graph initialization on first graph-backed tool call
//! - Precision mode detection (FullCanonical vs FocusPartial)
//! - Incremental graph refresh after lazy extraction writes
//! - Exposes SearchEngine (BFS/DFS/path) and ContextBuilder (callers/callees/source)
//! - Generation-based staleness detection via RuntimeInvalidation
//!
//! # Public API
//! - `ensure_initialized()`: build graph snapshot from DB (idempotent)
//! - `precision_info()`: return GraphPrecision { mode, edge_count }
//! - `provider()`: return &dyn GraphProvider — sole entry point for graph queries
//! - `is_graph_stale()` / `mark_graph_fresh()`: generation-based staleness tracking
//!
//! # Usage pattern
//! ```ignore
//! self.active.graph_runtime.ensure_initialized()?;
//! let cb = self.active.graph_runtime.provider().context_builder();
//! ```
//!
//! # Dependencies
//! - `atlas_engine::{GraphEngine, SearchEngine, ContextBuilder, GraphSnapshot}`
//! - `super::graph_provider::GraphProvider`
//! - `super::invalidation::RuntimeInvalidation`

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atlas_engine::{SourceExtractor, Store};

use super::closure_graph_provider::ClosureGraphProvider;
use super::graph_provider::GraphProvider;
use super::graph_state::GraphState;
use super::invalidation::RuntimeInvalidation;

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
}

// ── GraphRuntime ────────────────────────────────────────────────────────

/// Manages the in-memory call graph snapshot lifecycle.
///
/// Provides lazy initialization and incremental refresh of the
/// SearchEngine and ContextBuilder backed by GraphState.
pub struct GraphRuntime {
    /// Heap-allocated so `ClosureGraphProvider` can hold a stable raw pointer
    /// into it.  Both providers always see identical in-memory graph state.
    pub state: Box<GraphState>,
    /// Closure-scoped provider that shares the same heap allocation as `state`.
    closure_provider: ClosureGraphProvider,
    pub store: Arc<Store>,
    pub source_extractor: SourceExtractor,
    pub project_root: PathBuf,
    /// Provenance mode of graph edges (detected on first init).
    pub mode: Mutex<GraphMode>,
    /// Cached store index mode at graph init time.
    cached_index_mode: Mutex<Option<String>>,
    /// Shared invalidation counters for generation-based staleness detection.
    pub invalidation: Arc<RuntimeInvalidation>,
    /// Cached graph_generation at last refresh — compared against current
    /// to decide if a full rebuild is needed.
    pub last_graph_generation: AtomicU64,
}

impl GraphRuntime {
    pub fn new(
        store: Arc<Store>,
        source_extractor: SourceExtractor,
        project_root: PathBuf,
        invalidation: Arc<RuntimeInvalidation>,
    ) -> Self {
        let last_graph_signature = store.index_signature().unwrap_or_default();
        let last_graph_generation = invalidation.graph_generation.load(Ordering::Relaxed);
        let state = Box::new(GraphState {
            search: Mutex::new(None),
            context: Mutex::new(None),
            graph_initialized: std::sync::atomic::AtomicBool::new(false),
            last_graph_signature: Mutex::new(last_graph_signature),
            pending_graph_rebuild: Arc::new(std::sync::Mutex::new(None)),
        });
        let closure_provider = ClosureGraphProvider::from_box(&state);
        Self {
            state,
            closure_provider,
            store,
            source_extractor,
            project_root,
            mode: Mutex::new(GraphMode::FocusPartial),
            cached_index_mode: Mutex::new(None),
            invalidation,
            last_graph_generation: AtomicU64::new(last_graph_generation),
        }
    }

    /// Ensure the graph is initialized (lazy init on first query).
    /// Detects and caches the graph provenance mode on first init.
    /// Returns &SearchEngine or an error.
    pub fn ensure_initialized(&self) -> anyhow::Result<()> {
        let was_initialized = self
            .state
            .graph_initialized
            .load(std::sync::atomic::Ordering::Acquire);
        self.state
            .ensure_initialized(&self.store, &self.source_extractor, &self.project_root)?;
        if !was_initialized {
            let store = self.store.clone();
            self.detect_and_set_mode(&store);
        }
        if !self.state.is_initialized() {
            return Err(anyhow::anyhow!("graph not initialized"));
        }
        Ok(())
    }

    /// Returns true if the graph needs a full rebuild (generation changed).
    pub(crate) fn is_graph_stale(&self) -> bool {
        let current = self.invalidation.graph_generation.load(Ordering::Relaxed);
        current > self.last_graph_generation.load(Ordering::Relaxed)
    }

    /// Mark the graph as fresh (update cached generation to match current).
    pub(crate) fn mark_graph_fresh(&self) {
        self.last_graph_generation.store(
            self.invalidation.graph_generation.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    /// Detect the index precision mode from the store and cache it.
    pub fn detect_and_set_mode(&self, store: &Store) {
        let index_mode = store.read_index_mode().unwrap_or_default();
        *self.cached_index_mode.lock().unwrap() = Some(index_mode.clone());
        *self.mode.lock().unwrap() = if atlas_engine::is_rich_index_mode(&index_mode) {
            GraphMode::FullCanonical
        } else {
            GraphMode::FocusPartial
        };
    }

    /// Return precision metadata about the current graph.
    pub fn precision_info(&self) -> GraphPrecision {
        GraphPrecision {
            mode: *self.mode.lock().unwrap(),
            initialized: self.state.is_initialized(),
            edge_count: self.state.edge_count(),
        }
    }

    /// Returns the graph provider for the current scope.
    ///
    /// Dispatches based on [`GraphMode`]:
    /// - `FullCanonical` → `&self.state` (full graph)
    /// - `FocusPartial` → `&self.closure_provider` (closure-scoped)
    pub(crate) fn provider(&self) -> &dyn GraphProvider {
        match *self.mode.lock().unwrap() {
            GraphMode::FullCanonical => &*self.state,
            GraphMode::FocusPartial => &self.closure_provider,
        }
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
        let invalidation = Arc::new(RuntimeInvalidation::new());
        GraphRuntime::new(store, source_extractor, PathBuf::from("."), invalidation)
    }

    #[test]
    fn default_mode_is_focus_partial() {
        let gr = create_test_graph_runtime();
        assert_eq!(*gr.mode.lock().unwrap(), GraphMode::FocusPartial);
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
        assert_eq!(*gr.mode.lock().unwrap(), GraphMode::FocusPartial);
    }

    #[test]
    fn precision_info_full_canonical() {
        let mut gr = create_test_graph_runtime();
        *gr.mode.lock().unwrap() = GraphMode::FullCanonical;
        let info = gr.precision_info();
        assert_eq!(info.mode, GraphMode::FullCanonical);
    }

    #[test]
    fn ensure_initialized_sets_up_search_engine() {
        let mut gr = create_test_graph_runtime();
        let result = gr.ensure_initialized();
        assert!(result.is_ok(), "ensure_initialized should succeed");
        let gs_opt = gr.provider().graph_snapshot();
        assert!(
            gs_opt.is_some(),
            "graph_snapshot should be accessible after init"
        );
    }

    #[test]
    fn provider_trait_contract_holds() {
        let mut gr = create_test_graph_runtime();
        {
            let p = gr.provider();
            assert!(!p.is_initialized());
            assert!(p.graph_snapshot().is_none());
            assert!(p.graph_snapshot().is_none());
            assert_eq!(p.node_count(), 0);
            assert_eq!(p.edge_count(), 0);
        }

        gr.ensure_initialized().unwrap();
        let p = gr.provider();
        assert!(p.is_initialized());
        // Using an empty store yields zero nodes/edges — this is expected.
    }

    #[test]
    fn graph_not_stale_after_construction() {
        let gr = create_test_graph_runtime();
        assert!(!gr.is_graph_stale());
    }

    #[test]
    fn graph_stale_after_bump() {
        let mut gr = create_test_graph_runtime();
        let initial_gen = gr.last_graph_generation.load(Ordering::Relaxed);
        gr.invalidation
            .graph_generation
            .fetch_add(1, Ordering::Relaxed);
        assert!(gr.is_graph_stale());
        gr.mark_graph_fresh();
        assert!(!gr.is_graph_stale());
        assert!(gr.last_graph_generation.load(Ordering::Relaxed) > initial_gen);
    }
}
