//! Closure-scoped graph provider — second implementor of [`GraphProvider`].
//!
//! Shares the same [`GraphState`] as the full-canonical path.

use std::sync::Arc;

use atlas_engine::{ContextView, GraphEngine, SymbolId};

use super::graph_provider::GraphProvider;
use super::graph_state::GraphState;

pub(crate) struct ClosureGraphProvider {
    state: Arc<GraphState>,
}

impl ClosureGraphProvider {
    pub(crate) fn new(state: Arc<GraphState>) -> Self {
        Self { state }
    }
}

impl GraphProvider for ClosureGraphProvider {
    fn is_initialized(&self) -> bool {
        self.state.is_initialized()
    }

    fn graph_snapshot(&self) -> Option<Arc<GraphEngine>> {
        self.state.graph_snapshot()
    }

    fn build_context_for_symbol(
        &self,
        sid: &SymbolId,
        include_file_peers: bool,
    ) -> Option<Result<ContextView, anyhow::Error>> {
        self.state.build_context_for_symbol(sid, include_file_peers)
    }
}
