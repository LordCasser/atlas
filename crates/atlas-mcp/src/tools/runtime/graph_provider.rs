//! Graph provider trait — the contract between GraphRuntime and its graph backend.

use std::sync::Arc;

use atlas_engine::{ContextView, GraphEngine, SymbolId};

/// Minimal abstraction over graph backends.
pub(crate) trait GraphProvider {
    /// Whether the graph has been built.
    fn is_initialized(&self) -> bool;

    /// Return the full graph snapshot (Arc<GraphEngine>).
    fn graph_snapshot(&self) -> Option<Arc<GraphEngine>>;

    /// Build a context view for the given symbol.
    fn build_context_for_symbol(
        &self,
        sid: &SymbolId,
        include_file_peers: bool,
    ) -> Option<Result<ContextView, anyhow::Error>>;

    /// Total symbols in the graph, or 0 if not yet built.
    fn node_count(&self) -> usize;

    /// Total edges in the graph.
    fn edge_count(&self) -> usize;
}
