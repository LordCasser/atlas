//! Graph provider trait — the contract between GraphRuntime and its graph backend.
//!
//! Implementors:
//! - `GraphState` (production): full in-memory call graph from the DB.
//! - (future) `ClosureGraphState`: closure-scoped graph from focus extraction.

use atlas_engine::{ContextBuilder, SearchEngine};

/// Minimal abstraction over graph backends.
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
