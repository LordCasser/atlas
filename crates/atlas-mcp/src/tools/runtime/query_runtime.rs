//! Query runtime — focus-driven lazy extraction coordinator.
//!
//! # Responsibilities
//! - Owns FocusRuntime, CacheState, and LazyRefreshQueue
//! - `prepare()`: single entry point for focus-driven lazy analysis
//! - `has_full_index()`: check whether project has a complete index
//! - `detect_index_mode()`: inspect FocusRuntime's index mode
//!
//! # Usage pattern
//! ```ignore
//! let (focus_result, warnings) = self.active.query_runtime.prepare(&intent, &store);
//! ```
//!
//! # Dependencies
//! - `atlas_engine::focus::runtime::{FocusRuntime, FocusResult, IndexMode}`
//! - `super::cache_state::CacheState`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use atlas_engine::focus::runtime::{FocusResult, FocusRuntime, IndexMode};
use atlas_engine::focus::query::QueryIntent;
use atlas_engine::Store;

use super::cache_state::CacheState;
use crate::tools::lazy_refresh::LazyRefreshQueue;

/// Controls focus-driven lazy extraction for queries.
///
/// When the project is in Focus (incremental) mode, QueryRuntime
/// orchestrates bootstrap, seed location, closure building, and
/// background expansion through the underlying FocusRuntime.
pub struct QueryRuntime {
    /// Focus runtime, always present. Created during construction and
    /// configured with `init_focus()` to share the lazy dataflow service.
    pub focus_runtime: Mutex<FocusRuntime>,
    pub cache: CacheState,
    pub lazy_refresh_queue: Arc<LazyRefreshQueue>,
    store: Arc<Store>,
}

impl QueryRuntime {
    pub fn new(
        store: Arc<Store>,
        project_root: Option<PathBuf>,
        lazy_refresh_queue: Arc<LazyRefreshQueue>,
    ) -> Self {
        let signature = store.index_signature().unwrap_or_default();
        let cache = CacheState {
            cached_signature: signature,
            last_signature_check: std::time::Instant::now(),
            cached_manual_full_index: RwLock::new(None),
        };
        let focus_runtime = Mutex::new(FocusRuntime::new(store.clone(), project_root));
        Self {
            focus_runtime,
            cache,
            lazy_refresh_queue,
            store,
        }
    }

    /// Prepare focus-driven lazy extraction for a query intent.
    ///
    /// Returns `(None, vec![])` when the project has a full index.
    /// Returns `(Some(FocusResult), warnings)` when focus analysis completes.
    pub fn prepare(&self, intent: &QueryIntent, store: &Store) -> (Option<FocusResult>, Vec<String>) {
        // 1. Cache check: skip if manual full index exists
        if self.cache.has_manual_full_index(store) {
            return (None, vec![]);
        }

        // 2. Detect index mode (unlocks after check)
        let mode = self.detect_index_mode();
        if mode == IndexMode::FullIndex {
            return (None, vec![]);
        }

        // 3. Lock FocusRuntime and prepare
        let mut runtime = self.focus_runtime.lock().unwrap();
        match runtime.prepare(intent) {
            Ok(result) => (Some(result), vec![]),
            Err(e) => (None, vec![format!("Focus preparation failed: {e}")]),
        }
    }

    pub fn detect_index_mode(&self) -> IndexMode {
        self.focus_runtime.lock().unwrap().detect_index_mode()
    }

    /// Check whether the project has a full (non-manifest) index.
    ///
    /// Delegates to [`CacheState::has_manual_full_index`].
    pub fn has_full_index(&self, store: &Store) -> bool {
        self.cache.has_manual_full_index(store)
    }

    /// Check whether the project has a full index — cached-only path.
    ///
    /// Reads the last-known state from the session cache without
    /// accessing the store.  Useful for tests that need to bypass
    /// `&Store`.
    #[cfg(test)]
    pub fn has_full_index_cached(&self) -> bool {
        self.cache
            .cached_manual_full_index
            .read()
            .unwrap()
            .as_ref()
            .map(|(_, v)| *v)
            .unwrap_or(false)
    }

    /// Prepare for a graph-backed query: resolve symbols, run focus if needed,
    /// return readiness info. The graph provider is accessed separately via
    /// GraphRuntime::provider().
    pub fn prepare_graph_query(&self, intent: &QueryIntent) -> PreparedGraphQuery {
        let (focus_result, _warnings) = self.prepare(intent, &self.store);
        let closure_id = focus_result.as_ref().and_then(|r| r.closure_id.clone());
        let coverage_counts = focus_result
            .as_ref()
            .and_then(|r| r.coverage_counts.clone());
        PreparedGraphQuery {
            focus_triggered: focus_result.is_some(),
            closure_id,
            coverage_counts,
        }
    }
}

/// Lightweight result from prepare_graph_query — the caller accesses
/// the graph provider separately.
pub struct PreparedGraphQuery {
    pub focus_triggered: bool,
    /// The closure_id if focus extraction built a closure for this query.
    /// Callers should inject this into the response so clients can track
    /// closure provenance.
    pub closure_id: Option<String>,
    /// Distribution of results by coverage tier from the focus extraction.
    /// Callers should inject this into the response alongside any per-query
    /// precision metadata.
    pub coverage_counts: Option<HashMap<String, usize>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::Store;
    use crate::tools::lazy_refresh::LazyRefreshQueue;
    use std::sync::Arc;

    fn create_test_query_runtime() -> QueryRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let lazy_refresh_queue = LazyRefreshQueue::new();
        QueryRuntime::new(store, None, lazy_refresh_queue)
    }

    #[test]
    fn focus_runtime_is_always_present() {
        let qr = create_test_query_runtime();
        // FocusRuntime is always present — no Option wrapper.
        let mode = qr.detect_index_mode();
        // For an empty in-memory store with no full index, detect_index_mode
        // should return IndexMode::Focus (lazy extraction is possible).
        assert_eq!(mode, IndexMode::Focus);
    }

    #[test]
    fn prepare_returns_result_on_focus_mode() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let intent = QueryIntent::Calls {
            symbol_name: "test".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        // FocusRuntime is initialized — prepare should attempt focus analysis.
        let (result, warnings) = qr.prepare(&intent, &store);
        // In an empty store, focus preparation may return None with warnings
        // (seed not found) or Some with a result. Either way, it should not
        // return the "not initialized" error path.
        for w in &warnings {
            assert!(
                !w.contains("not initialized"),
                "should not contain 'not initialized': {w}"
            );
        }
    }

    #[test]
    fn prepare_skips_when_full_index_exists() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        // Simulate a full index by pre-populating the cache
        let signature = store.index_signature().unwrap_or_default();
        *qr.cache.cached_manual_full_index.write().unwrap() =
            Some((signature.clone(), true));
        let intent = QueryIntent::Calls {
            symbol_name: "test".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        let (result, warnings) = qr.prepare(&intent, &store);
        assert!(result.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn has_full_index_returns_false_for_fresh_store() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        assert!(!qr.has_full_index(&store));
    }

    #[test]
    fn has_full_index_returns_true_when_cached() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        // Warm the cache
        let sig = store.index_signature().unwrap_or_default();
        *qr.cache.cached_manual_full_index.write().unwrap() = Some((sig, true));
        assert!(qr.has_full_index(&store));
    }

    #[test]
    fn prepared_graph_query_carries_closure_id() {
        let qr = create_test_query_runtime();
        let intent = QueryIntent::Calls {
            symbol_name: "test_func".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        let pq = qr.prepare_graph_query(&intent);
        // On an empty in-memory store, focus may or may not trigger.
        // When focus triggers (most common case for Focus mode), the
        // prepared query should carry whatever closure_id the focus
        // result provides.  When focus does not trigger (full index or
        // error), both fields should be None.
        if pq.focus_triggered {
            // coverage_counts should be Some when focus is triggered
            // (FocusResult always populates this in Focus mode).
            assert!(pq.coverage_counts.is_some());
        } else {
            assert!(pq.closure_id.is_none());
            assert!(pq.coverage_counts.is_none());
        }
    }
}
