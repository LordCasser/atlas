//! Query runtime — focus-driven lazy extraction coordinator.
//!
//! # Responsibilities
//! - Owns FocusRuntime, CacheState, and LazyRefreshQueue
//! - `prepare()`: single entry point for focus-driven lazy analysis
//! - `has_repo_cache_for()`: check whether a finalized repo cache satisfies a query need
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

use atlas_engine::FocusMaterialize;
use atlas_engine::IncludeRoot;
use atlas_engine::Store;
use atlas_engine::focus::query::QueryIntent;
use atlas_engine::focus::runtime::{AccessStrategy, FocusResult, FocusRuntime};
use atlas_engine::{FileId, QueryNeed};

use super::cache_state::CacheState;
use crate::tools::lazy_refresh::LazyRefreshQueue;

/// Controls focus-driven lazy extraction for queries.
///
/// When the project is in Focus (incremental) mode, QueryRuntime
/// orchestrates bootstrap, seed location, closure building, and
/// background expansion through the underlying FocusRuntime.
pub struct QueryRuntime {
    /// Focus runtime with required Focus materialize (construction-time inject).
    ///
    /// Private on purpose: all access goes through the delegate methods below
    /// so handler source never takes `focus_runtime.lock()` directly (DEBT-8
    /// handler purity). The field is still reachable from within this module.
    focus_runtime: Mutex<FocusRuntime>,
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
            cached_repo_cache: RwLock::new(None),
        };
        let focus_runtime = Mutex::new(FocusRuntime::new(store.clone(), project_root, materialize));
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
    /// Returns `(None, vec![])` when a finalized repo cache satisfies the intent.
    /// Returns `(Some(FocusResult), warnings)` when focus analysis completes.
    pub fn prepare(
        &self,
        intent: &QueryIntent,
        store: &Store,
        include_roots: Vec<IncludeRoot>,
    ) -> (Option<FocusResult>, Vec<String>) {
        // 1. QueryNeed-aware repo-cache check.
        if self
            .cache
            .has_repo_cache_for(store, intent.required_analysis())
        {
            return (None, vec![]);
        }

        // 2. Detect index mode (unlocks after check)
        let mode = self.detect_access_strategy(intent.required_analysis());
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

    pub fn detect_access_strategy(&self, need: QueryNeed) -> AccessStrategy {
        self.focus_runtime
            .lock()
            .unwrap()
            .detect_access_strategy(need)
    }

    /// Enqueue background file-focused warming without building a foreground
    /// closure (search uses this after a bounded provisional pass; the MCP
    /// response gate does not publish that pass while work is pending).
    ///
    /// Thin delegate over [`FocusRuntime::enqueue_file_focus_warm`] so handler
    /// source never takes `focus_runtime.lock()` directly (DEBT-8 purity).
    pub fn enqueue_file_focus_warm(
        &self,
        file_ids: &[FileId],
    ) -> anyhow::Result<Option<FocusResult>> {
        self.focus_runtime
            .lock()
            .unwrap()
            .enqueue_file_focus_warm(file_ids)
    }

    /// Start Focus bootstrap without waiting for project-wide discovery.
    pub fn ensure_focus_started(&self) {
        self.focus_runtime.lock().unwrap().ensure_started();
    }

    /// Whether background Tier 0 has discovered the complete project inventory.
    pub fn is_tier0_complete(&self) -> bool {
        self.focus_runtime.lock().unwrap().is_tier0_complete()
    }

    /// Whether the Focus runtime's materialize has a structural rebuilder.
    ///
    /// Audit probe for the construction-time invariant test (DEBT-8 purity:
    /// tests must not lock `focus_runtime` directly).
    #[allow(dead_code)] // exercised by the construction-time invariant test only
    pub fn focus_materialize_has_structural_rebuilder(&self) -> bool {
        self.focus_runtime
            .lock()
            .unwrap()
            .materialize()
            .has_structural_rebuilder()
    }

    /// Whether the Focus runtime's materialize shares the same Arc stack as
    /// `other`. Symmetric with [`FocusMaterialize::same_stack_as`]; audit probe
    /// for the construction-time invariant test.
    #[allow(dead_code)] // exercised by the construction-time invariant test only
    pub fn focus_materialize_same_stack_as(&self, other: &FocusMaterialize) -> bool {
        self.focus_runtime
            .lock()
            .unwrap()
            .materialize()
            .same_stack_as(other)
    }

    /// Drain project-wide background-built files into the lazy refresh queue.
    ///
    /// `FocusRuntime` owns the shared job tracker, so this feed covers jobs from
    /// prior query snapshots and file-focused warming as well as the current
    /// request. The tracker retains per-job history for `resume_query` while
    /// exposing each file only once through this refresh feed.
    ///
    /// Returns the files recorded to the queue. Safe to call on every graph
    /// refresh; the tracker drain and queue are both deduplication-aware.
    pub fn record_background_built_files(&self) -> Vec<FileId> {
        let built = self
            .focus_runtime
            .lock()
            .unwrap()
            .take_background_refresh_files();
        if !built.is_empty() {
            self.lazy_refresh_queue.record_lazy_writes(&built);
        }
        built
    }

    /// Check whether a finalized whole-repository Index satisfies `need`.
    pub fn has_repo_cache_for(&self, store: &Store, need: QueryNeed) -> bool {
        self.cache.has_repo_cache_for(store, need)
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

    fn mark_finalized_structural_index(store: &Store) {
        store.set_metadata("last_index_time", "1").unwrap();
        store
            .set_metadata(
                "indexed_scope",
                &serde_json::json!({ "include": [], "exclude": [] }).to_string(),
            )
            .unwrap();
        store
            .set_metadata("indexed_pipeline_grade", "structural")
            .unwrap();
    }

    #[test]
    fn focus_runtime_is_always_present() {
        let qr = create_test_query_runtime();
        // FocusRuntime is always present — no Option wrapper.
        let mode = qr.detect_access_strategy(QueryNeed::CallGraph);
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
        *qr.cache.cached_repo_cache.write().unwrap() =
            Some((signature.clone(), QueryNeed::CallGraph, true));
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
    fn repo_cache_returns_false_for_fresh_store() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        assert!(!qr.has_repo_cache_for(&store, QueryNeed::CallGraph));
    }

    #[test]
    fn repo_cache_returns_true_when_cached() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        // Warm the cache
        let sig = store.index_signature().unwrap_or_default();
        *qr.cache.cached_repo_cache.write().unwrap() = Some((sig, QueryNeed::CallGraph, true));
        assert!(qr.has_repo_cache_for(&store, QueryNeed::CallGraph));
    }

    #[test]
    fn graph_repo_cache_requires_finalized_structural_index() {
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
            !qr.has_repo_cache_for(&store, QueryNeed::CallGraph),
            "unfinalized focus-written rich layers must not disable focus"
        );

        mark_finalized_structural_index(&store);
        assert!(
            qr.has_repo_cache_for(&store, QueryNeed::CallGraph),
            "CLI-finalized rich index should disable focus"
        );

        store
            .set_metadata("indexed_pipeline_grade", "manifest")
            .unwrap();
        assert!(
            !qr.has_repo_cache_for(&store, QueryNeed::CallGraph),
            "cache signature must observe changes to finalized pipeline authority"
        );
    }

    #[test]
    fn structural_full_cache_does_not_skip_dataflow_focus() {
        let store = Arc::new(Store::open_in_memory().unwrap());
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
        mark_finalized_structural_index(&store);

        let materialize = atlas_engine::FocusMaterialize::open(store.clone(), None);
        let runtime = QueryRuntime::new(store.clone(), None, materialize, LazyRefreshQueue::new());
        assert!(runtime.has_repo_cache_for(&store, QueryNeed::CallGraph));

        let intent = QueryIntent::TraceVariable {
            file_id,
            line: 1,
            column: 1,
            max_depth: 3,
        };
        let (result, warnings) = runtime.prepare(&intent, &store, Vec::new());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            result.map(|result| result.access),
            Some(AccessStrategy::Focus),
            "structural cache must seed, not suppress, dataflow Focus"
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
