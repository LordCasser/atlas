//! Cache state — index signature and manual-full-index detection cache.
//!
//! Owned by QueryRuntime. Provides:
//! - `has_manual_full_index(store)`: cached check for full index existence
//! - `cached_signature`: current index signature for change detection
//! - `last_signature_check`: timestamp of last signature comparison
//! - `invalidate_manual_full_index_cache()`: called after re-index

use std::sync::RwLock;
use std::time::Instant;

use atlas_engine::Store;
use atlas_engine::is_rich_index_mode;

/// Index-signature and manual-full-index detection cache.
pub(crate) struct CacheState {
    /// Cached index signature to avoid per-request COUNT queries.
    pub(crate) cached_signature: String,
    /// When the cached signature was last checked (avoids re-query within cooldown).
    pub(crate) last_signature_check: Instant,
    /// Cached result of `has_manual_full_index()` keyed by index signature.
    /// `None` means not yet checked; signature changes force re-check.
    pub(crate) cached_manual_full_index: RwLock<Option<(String, bool)>>,
}

impl CacheState {
    /// Detect whether the current database already has a reusable rich index.
    ///
    /// This lets MCP avoid lazy preparse work when the active store is already
    /// structural/full, regardless of whether that index was built by CLI, TUI,
    /// or MCP.
    ///
    /// The result is cached for the lifetime of the session; callers that
    /// trigger a re-index (MCP `index` tool) should invalidate this cache
    /// after completion.
    pub(crate) fn has_manual_full_index(&self, store: &Store) -> bool {
        let signature = store.index_signature().unwrap_or_default();
        if let Some((cached_signature, cached)) =
            &*self
                .cached_manual_full_index
                .read()
                .unwrap_or_else(|e| e.into_inner())
            && *cached_signature == signature
        {
            return *cached;
        }
        let index_mode = store
            .read_index_mode()
            .unwrap_or_else(|_| "unknown".to_string());
        let result = is_rich_index_mode(&index_mode);
        *self
            .cached_manual_full_index
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some((signature, result));
        result
    }

    /// Invalidate the cached manual-full-index flag.
    ///
    /// Called after MCP `index` completes, so the next search/trace query
    /// re-checks the actual layer distribution.
    pub(crate) fn invalidate_manual_full_index_cache(&self) {
        *self
            .cached_manual_full_index
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}
