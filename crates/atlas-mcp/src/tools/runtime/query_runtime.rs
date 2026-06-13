use std::sync::{Arc, Mutex, RwLock};

use atlas_engine::focus::runtime::{FocusResult, FocusRuntime, IndexMode};
use atlas_engine::focus::query::QueryIntent;
use atlas_engine::Store;

use crate::tools::cache_state::CacheState;
use crate::tools::lazy_refresh::LazyRefreshQueue;

/// Controls focus-driven lazy extraction for queries.
///
/// When the project is in Focus (incremental) mode, QueryRuntime
/// orchestrates bootstrap, seed location, closure building, and
/// background expansion through the underlying FocusRuntime.
pub struct QueryRuntime {
    /// Focus runtime, initialized lazily via `init_focus()`.
    /// None until explicitly initialized (preserves old ToolRouter behavior).
    pub focus_runtime: Option<Mutex<FocusRuntime>>,
    pub cache: CacheState,
    pub lazy_refresh_queue: Arc<LazyRefreshQueue>,
}

impl QueryRuntime {
    pub fn new(
        store: Arc<Store>,
        lazy_refresh_queue: Arc<LazyRefreshQueue>,
    ) -> Self {
        let signature = store.index_signature().unwrap_or_default();
        let cache = CacheState {
            cached_signature: signature,
            last_signature_check: std::time::Instant::now(),
            cached_manual_full_index: RwLock::new(None),
        };
        Self {
            // focus_runtime starts as None — same as old ToolRouter::new_empty/new.
            // init_focus() must be called to activate focus-driven extraction.
            focus_runtime: None,
            cache,
            lazy_refresh_queue,
        }
    }

    /// Prepare focus-driven lazy extraction for a query intent.
    ///
    /// Returns `(None, vec![])` when the project has a full index or
    /// FocusRuntime is not initialized.  Returns `(Some(FocusResult), warnings)`
    /// when focus analysis completes.
    pub fn prepare(&self, intent: &QueryIntent, store: &Store) -> (Option<FocusResult>, Vec<String>) {
        // 1. Cache check: skip if manual full index exists
        if self.cache.has_manual_full_index(store) {
            return (None, vec![]);
        }

        // 2. Check focus_runtime initialized
        let fr = match &self.focus_runtime {
            Some(fr) => fr,
            None => {
                return (
                    None,
                    vec!["No full index and FocusRuntime not initialized.".to_string()],
                );
            }
        };

        // 3. Lock FocusRuntime, detect mode, prepare
        let mut runtime = fr.lock().unwrap();
        let mode = runtime.detect_index_mode();
        if mode == IndexMode::FullIndex {
            return (None, vec![]);
        }
        match runtime.prepare(intent) {
            Ok(result) => (Some(result), vec![]),
            Err(e) => (None, vec![format!("Focus preparation failed: {e}")]),
        }
    }

    #[allow(dead_code)]
    pub fn detect_index_mode(&self) -> Option<IndexMode> {
        self.focus_runtime
            .as_ref()
            .map(|fr| fr.lock().unwrap().detect_index_mode())
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
        QueryRuntime::new(store, lazy_refresh_queue)
    }

    #[test]
    fn focus_runtime_starts_none() {
        let qr = create_test_query_runtime();
        assert!(qr.focus_runtime.is_none());
    }

    #[test]
    fn detect_index_mode_returns_none_when_focus_not_initialized() {
        let qr = create_test_query_runtime();
        assert!(qr.detect_index_mode().is_none());
    }

    #[test]
    fn prepare_returns_none_when_focus_not_initialized() {
        let qr = create_test_query_runtime();
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let intent = QueryIntent::Calls {
            symbol_name: "test".into(),
            file_id: None,
            symbol_id: None,
        };
        let (result, _warnings) = qr.prepare(&intent, &store);
        assert!(result.is_none());
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
}
