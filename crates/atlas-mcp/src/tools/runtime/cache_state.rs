//! Cache state — index signature and manual-full-index detection cache.
//!
//! Owned by QueryRuntime. Provides:
//! - `has_manual_full_index(store)`: cached check for CLI-finalized rich index existence
//! - `cached_signature`: current index signature for change detection
//! - `last_signature_check`: timestamp of last signature comparison

use std::sync::{Mutex, RwLock};
use std::time::Instant;

use atlas_engine::Store;
use atlas_engine::is_rich_index_mode;

/// Index-signature and manual-full-index detection cache.
pub(crate) struct CacheState {
    /// Cached index signature to avoid per-request COUNT queries.
    pub(crate) cached_signature: Mutex<String>,
    /// When the cached signature was last checked (avoids re-query within cooldown).
    pub(crate) last_signature_check: Mutex<Instant>,
    /// Cached result of `has_manual_full_index()` keyed by index signature.
    /// `None` means not yet checked; signature changes force re-check.
    pub(crate) cached_manual_full_index: RwLock<Option<(String, bool)>>,
}

impl CacheState {
    /// Detect whether the current database already has a reusable rich index
    /// finalized by an explicit CLI/TUI indexing run.
    ///
    /// Focus writes can produce rich per-file layers in a small local closure.
    /// Those layers must not make later MCP queries believe the whole project
    /// is fully indexed, so this check requires both rich extraction state and
    /// index-finalization metadata.
    ///
    /// The result is cached by store signature; signature changes force
    /// re-detection.
    //
    // ⚠️ Mirrors the core check in FocusRuntime::detect_index_mode()
    //    (crates/atlas-engine/src/focus/runtime.rs).  Both require
    //    `is_rich_index_mode() && last_index_time.is_some()`.  Keep in sync.
    pub(crate) fn has_manual_full_index(&self, store: &Store) -> bool {
        let signature = store.index_signature().unwrap_or_default();
        if let Some((cached_signature, cached)) = &*self
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
        let finalized = store
            .get_metadata("last_index_time")
            .ok()
            .flatten()
            .is_some();
        let result = finalized && is_rich_index_mode(&index_mode);
        *self
            .cached_manual_full_index
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some((signature, result));
        result
    }
}
