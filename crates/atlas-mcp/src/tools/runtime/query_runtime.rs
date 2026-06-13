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

    /// Prepare for a query. Delegates to FocusRuntime.
    /// Returns None if FullIndex mode, no intent, or focus_runtime not initialized.
    /// MIRRORS: ToolRouter::prepare_focus_query() in mod.rs
    pub fn prepare(&self, intent: &QueryIntent) -> (Option<FocusResult>, Vec<String>) {
        let fr = match &self.focus_runtime {
            Some(fr) => fr,
            None => return (None, vec![]),
        };
        let mut runtime = fr.lock().unwrap();
        let mode = runtime.detect_index_mode();
        match mode {
            IndexMode::FullIndex => (None, vec![]),
            IndexMode::Focus => match runtime.prepare(intent) {
                Ok(result) => (Some(result), vec![]),
                Err(e) => (None, vec![format!("focus prepare error: {e}")]),
            },
        }
    }

    pub fn detect_index_mode(&self) -> Option<IndexMode> {
        self.focus_runtime
            .as_ref()
            .map(|fr| fr.lock().unwrap().detect_index_mode())
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
}
