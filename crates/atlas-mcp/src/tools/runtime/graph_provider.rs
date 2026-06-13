//! Graph provider trait — the contract between GraphRuntime and its graph backend.
//!
//! # Implementors
//! - `GraphState` (production): full in-memory graph from SQLite DB
//! - (future) `ClosureGraphProvider`: closure-scoped graph from focus extraction
//!
//! # Why a trait?
//! The trait separates the graph *query* interface from the graph *lifecycle*
//! management. GraphRuntime handles lifecycle (init, refresh, mode detection);
//! GraphProvider handles queries (search, context, counts). When closure-scoped
//! graphs are implemented, switching backends is a one-line change in GraphRuntime.

use atlas_engine::{ContextBuilder, SearchEngine};

/// Minimal abstraction over graph backends.
#[allow(dead_code)] // Documented contract; integrated when ClosureGraphProvider is added
pub(crate) trait GraphProvider {
    /// Whether the graph has been built.
    fn is_initialized(&self) -> bool;

    /// Traversal engine (BFS, shortest-path, impact radius).
    fn search_engine(&self) -> Option<&SearchEngine>;

    /// Context builder (callers, callees, source snippets).
    fn context_builder(&self) -> Option<&ContextBuilder>;

    /// Total symbols in the graph, or 0 if not yet built.
    fn node_count(&self) -> usize;

    /// Total edges in the graph.
    fn edge_count(&self) -> usize;
}
