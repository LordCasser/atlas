//! Query runtime — focus-driven lazy extraction coordinator.
//!
//! # Responsibilities
//! - Owns FocusRuntime, CacheState, and LazyRefreshQueue
//! - `prepare()`: single entry point for focus-driven lazy analysis
//! - `has_full_index()`: check whether project has a complete index
//! - `detect_access_strategy()`: inspect FocusRuntime's index mode
//!
//! # Usage pattern
//! ```ignore
//! let (focus_result, warnings) =
//!     self.active.query_runtime.prepare(&intent, &store, include_roots);
//! ```
//!
//! # Dependencies
//! - `atlas_engine::focus::runtime::{FocusRuntime, FocusResult, AccessStrategy}`
//! - `super::cache_state::CacheState`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use atlas_engine::IncludeRoot;
use atlas_engine::Store;
use atlas_engine::FocusMaterialize;
use atlas_engine::focus::query::QueryIntent;
use atlas_engine::focus::runtime::{FocusResult, FocusRuntime, AccessStrategy};

use super::cache_state::CacheState;
use crate::tools::lazy_refresh::LazyRefreshQueue;

/// Controls focus-driven lazy extraction for queries.
///
/// When the project is in Focus (incremental) mode, QueryRuntime
/// orchestrates bootstrap, seed location, closure building, and
/// background expansion through the underlying FocusRuntime.
pub struct QueryRuntime {
    /// Focus runtime with required Focus materialize (construction-time inject).
    pub focus_runtime: Mutex<FocusRuntime>,
    pub cache: CacheState,
    pub lazy_refresh_queue: Arc<LazyRefreshQueue>,
    store: Arc<Store>,
}

impl QueryRuntime {
    pub fn new(
        store: Arc<Store>,
        project_root: Option<PathBuf>,
        materialize: FocusMaterialize,
        lazy_refresh_queue: Arc<LazyRefreshQueue>,
    ) -> Self {
        let signature = store.index_signature().unwrap_or_default();
        let cache = CacheState {
            cached_signature: Mutex::new(signature),
            last_signature_check: Mutex::new(std::time::Instant::now()),
            cached_manual_full_index: RwLock::new(None),
        };
        let focus_runtime = Mutex::new(FocusRuntime::new(
            store.clone(),
            project_root,
            materialize,
        ));
        Self {
            focus_runtime,
            cache,
            lazy_refresh_queue,
            store,
        }
    }

    /// Prepare focus-driven lazy extraction for a query intent.
    ///
    /// `include_roots` are request-scoped angle-include roots (validated and
    /// normalised by the MCP layer). They are copied into the foreground and
    /// background focus windows for this query, so the background scheduler never
    /// observes mutable per-query state from another request.
    ///
    /// Returns `(None, vec![])` when the project has a full index.
    /// Returns `(Some(FocusResult), warnings)` when focus analysis completes.
    pub fn prepare(
        &self,
        intent: &QueryIntent,
        store: &Store,
        include_roots: Vec<IncludeRoot>,
    ) -> (Option<FocusResult>, Vec<String>) {
        // 1. Cache check: skip if manual full index exists
        if self.cache.has_manual_full_index(store) {
            return (None, vec![]);
        }

        // 2. Detect index mode (unlocks after check)
        let mode = self.detect_access_strategy();
        if mode == AccessStrategy::FullCache {
            return (None, vec![]);
        }

        // 3. Lock FocusRuntime and prepare.
        //
        // The Mutex is held across the synchronous closure build so foreground
        // focus writes for one project remain ordered. Async handlers run this
        // blocking work on dedicated worker threads; unrelated store and graph
        // queries do not acquire this lock.
        //
        let mut runtime = self.focus_runtime.lock().unwrap();
        match runtime.prepare(intent, include_roots) {
            Ok(result) => (Some(result), vec![]),
            Err(e) => (None, vec![format!("Focus preparation failed: {e}")]),
        }
    }

    pub fn detect_access_strategy(&self) -> AccessStrategy {
        self.focus_runtime.lock().unwrap().detect_access_strategy()
    }

    /// Check whether the project has a full (non-manifest) index.
    ///
    /// Delegates to [`CacheState::has_manual_full_index`].
    pub fn has_full_index(&self, store: &Store) -> bool {
        self.cache.has_manual_full_index(store)
    }

    /// Prepare for a graph-backed query: resolve symbols, run focus if needed,
    /// return readiness info. The graph provider is accessed separately via
    /// GraphRuntime::provider().
    #[allow(dead_code)] // wired in handle_callers (P0-F2-A); future handlers coming
    pub fn prepare_graph_query(&self, intent: &QueryIntent) -> PreparedGraphQuery {
        let (focus_result, _warnings) = self.prepare(intent, &self.store, Vec::new());
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
#[allow(dead_code)] // wired in handle_callers (P0-F2-A); future handlers coming
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
    use crate::tools::lazy_refresh::LazyRefreshQueue;
    use atlas_engine::{FactCoverage, FileId, FileInfo, Language, ParseStatus, Store};
    use std::sync::Arc;

    fn create_test_query_runtime() -> QueryRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let materialize = atlas_engine::FocusMaterialize::open(store.clone(), None);
        let lazy_refresh_queue = LazyRefreshQueue::new();
        QueryRuntime::new(store, None, materialize, lazy_refresh_queue)
    }

    #[test]
    fn focus_runtime_is_always_present() {
        let qr = create_test_query_runtime();
        // FocusRuntime is always present — no Option wrapper.
        let mode = qr.detect_access_strategy();
        // For an empty in-memory store with no full index, detect_access_strategy
        // should return AccessStrategy::Focus (lazy extraction is possible).
        assert_eq!(mode, AccessStrategy::Focus);
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
        let (_result, warnings) = qr.prepare(&intent, &store, Vec::new());
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
        *qr.cache.cached_manual_full_index.write().unwrap() = Some((signature.clone(), true));
        let intent = QueryIntent::Calls {
            symbol_name: "test".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        let (result, warnings) = qr.prepare(&intent, &store, Vec::new());
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
    fn has_full_index_requires_finalized_rich_index() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        let file_id = FileId::generate("src/main.c");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/main.c".into(),
                language: Language::C,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "hash",
                "complete",
                FactCoverage::default(),
            )
            .unwrap();

        assert!(
            !qr.has_full_index(&store),
            "unfinalized focus-written rich layers must not disable focus"
        );

        store.set_metadata("last_index_time", "1").unwrap();
        assert!(
            qr.has_full_index(&store),
            "CLI-finalized rich index should disable focus"
        );
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
