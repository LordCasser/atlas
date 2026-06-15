//! Closure-scoped graph provider — second implementor of [`GraphProvider`].
//!
//! # Design
//! `ClosureGraphProvider` shares the **same** heap-allocated [`GraphState`]
//! as the `FullCanonical` path via a raw pointer.  This avoids cloning
//! (the engine types contain `RwLock` and are not `Clone`) and guarantees
//! that graph refreshes are visible through both providers immediately.
//!
//! The raw pointer points into the `Box<GraphState>` owned by
//! [`GraphRuntime`]; it is valid for the entire lifetime of `GraphRuntime`.
//!
//! # Future
//! When closure-scoped snapshots are implemented, this struct will own an
//! independent [`GraphState`] built from a subset of edges.

use atlas_engine::{ContextBuilder, SearchEngine};

use super::graph_provider::GraphProvider;
use super::graph_state::GraphState;

/// Graph provider for focus/closure mode queries.
///
/// Shares the same underlying [`GraphState`] as `FullCanonical` mode.
/// The dispatch via [`GraphProvider`] proves the trait supports
/// polymorphism and paves the way for true closure-scoped snapshots.
pub(crate) struct ClosureGraphProvider {
    /// Raw pointer to the heap-allocated `GraphState` in `GraphRuntime`.
    ///
    /// # Safety
    /// The pointed-to `GraphState` lives inside a `Box` that is dropped
    /// together with `GraphRuntime`.  Since `ClosureGraphProvider` is a
    /// field of `GraphRuntime`, the pointer is always valid when accessed.
    ///
    /// SAFETY: `GraphRuntime` must **never** derive `Clone` or `mem::replace`
    /// the `Box<GraphState>`.  The raw pointer points to the stable heap
    /// allocation; it is valid for the entire lifetime of `GraphRuntime`.
    state: *const GraphState,
}

// SAFETY: `*const GraphState` is `Send` because `GraphState` is `Send`
// (it contains `Arc<…>` and `Mutex<…>`, both `Send`).
unsafe impl Send for ClosureGraphProvider {}
// SAFETY: `*const GraphState` is `Sync` because `GraphState` is `Sync`
// (the inner `Mutex` and `RwLock` provide internal synchronisation).
unsafe impl Sync for ClosureGraphProvider {}

impl ClosureGraphProvider {
    /// Create a closure provider that shares the given heap-allocated state.
    ///
    /// The `state` parameter is a `Box<GraphState>` — its heap address is
    /// stable across moves of the `Box` itself.  The returned provider holds
    /// a raw pointer into the boxed allocation.
    pub(crate) fn from_box(state: &Box<GraphState>) -> Self {
        Self {
            state: &**state as *const GraphState,
        }
    }

    /// Dereference the raw pointer to get a `&GraphState`.
    ///
    /// # Safety
    /// The caller must ensure the original `Box<GraphState>` has not been
    /// dropped.  This is guaranteed by the fact that `GraphRuntime` owns
    /// both the `Box` and this `ClosureGraphProvider`.
    #[inline]
    fn state_ref(&self) -> &GraphState {
        // SAFETY: the pointer was created from a live `Box<GraphState>`
        // owned by `GraphRuntime`, which outlives this provider.
        unsafe { &*self.state }
    }
}

impl GraphProvider for ClosureGraphProvider {
    fn is_initialized(&self) -> bool {
        self.state_ref().is_initialized()
    }

    fn search_engine(&self) -> Option<&SearchEngine> {
        self.state_ref().search_engine()
    }

    fn context_builder(&self) -> Option<&ContextBuilder> {
        self.state_ref().context_builder()
    }

    fn node_count(&self) -> usize {
        self.state_ref().node_count()
    }

    fn edge_count(&self) -> usize {
        self.state_ref().edge_count()
    }
}
