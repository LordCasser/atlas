//! Cache state — index signature and finalized repo-cache detection cache.
//!
//! Owned by QueryRuntime. Provides:
//! - `has_repo_cache_for(store, need)`: cached check for a finalized
//!   whole-project cache that satisfies the current query
//! - `cached_signature`: current index signature for change detection
//! - `last_signature_check`: timestamp of last signature comparison

use std::sync::{Mutex, RwLock};
use std::time::Instant;

use atlas_engine::{QueryNeed, Store, has_finalized_repo_cache_for};

/// Index-signature and finalized repo-cache detection cache.
pub(crate) struct CacheState {
    /// Cached index signature to avoid per-request COUNT queries.
    pub(crate) cached_signature: Mutex<String>,
    /// When the cached signature was last checked (avoids re-query within cooldown).
    pub(crate) last_signature_check: Mutex<Instant>,
    /// Cached result of `has_repo_cache_for()` keyed by index signature
    /// and query need.
    /// `None` means not yet checked; signature changes force re-check.
    pub(crate) cached_repo_cache: RwLock<Option<(String, QueryNeed, bool)>>,
}

impl CacheState {
    /// Detect whether the current database has a finalized whole-project Index
    /// that already satisfies `need`.
    ///
    /// Focus writes can produce rich per-file layers in a small local closure.
    /// Those layers must not make later MCP queries believe the whole project
    /// is fully indexed. Scoped Index output is also only a reusable fact base,
    /// and a structural Index cannot satisfy a dataflow query by itself.
    ///
    /// The result is cached by store signature; signature changes force
    /// re-detection.
    //
    pub(crate) fn has_repo_cache_for(&self, store: &Store, need: QueryNeed) -> bool {
        let signature = store.index_signature().unwrap_or_default();
        if let Some((cached_signature, cached_need, cached)) = &*self
            .cached_repo_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            && *cached_signature == signature
            && *cached_need == need
        {
            return *cached;
        }
        let result = has_finalized_repo_cache_for(store, need);
        *self
            .cached_repo_cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some((signature, need, result));
        result
    }
}
