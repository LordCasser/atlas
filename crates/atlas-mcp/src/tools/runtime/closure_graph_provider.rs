//! Closure-scoped graph provider — second implementor of [`GraphProvider`].
//!
//! Shares the same heap-allocated [`GraphState`] as the full-canonical path
//! via a raw pointer.  GraphState uses interior mutability so the raw-pointer
//! const access is safe.

use std::sync::Arc;

use atlas_engine::{ContextView, GraphEngine, SymbolId};

use super::graph_provider::GraphProvider;
use super::graph_state::GraphState;

pub(crate) struct ClosureGraphProvider {
    state: *const GraphState,
}

// SAFETY: GraphState contains Mutex+AtomicBool (all Send+Sync).
unsafe impl Send for ClosureGraphProvider {}
unsafe impl Sync for ClosureGraphProvider {}

impl ClosureGraphProvider {
    pub(crate) fn from_box(state: &Box<GraphState>) -> Self {
        Self { state: &**state as *const GraphState }
    }

    #[inline]
    fn state_ref(&self) -> &GraphState {
        // SAFETY: pointer from live Box<GraphState> owned by GraphRuntime.
        unsafe { &*self.state }
    }
}

impl GraphProvider for ClosureGraphProvider {
    fn is_initialized(&self) -> bool {
        self.state_ref().is_initialized()
    }

    fn graph_snapshot(&self) -> Option<Arc<GraphEngine>> {
        self.state_ref().graph_snapshot()
    }

    fn build_context_for_symbol(
        &self,
        sid: &SymbolId,
        include_file_peers: bool,
    ) -> Option<Result<ContextView, anyhow::Error>> {
        self.state_ref().build_context_for_symbol(sid, include_file_peers)
    }

    fn node_count(&self) -> usize {
        self.state_ref().node_count()
    }

    fn edge_count(&self) -> usize {
        self.state_ref().edge_count()
    }
}
