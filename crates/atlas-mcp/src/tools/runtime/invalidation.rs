//! Runtime invalidation — centralized generation counters for cache coherence.
//!
//! # Responsibilities
//! - Provides monotonically-increasing generation numbers for:
//!   - `graph_generation`: bumped when graph topology changes (overlay mutations,
//!     index completion, lazy structural writes).
//!   - `overlay_generation`: bumped when user annotations change (fp_dispatches
//!     add/delete, domain_rules add/delete).
//!   - `analysis_config_generation`: bumped when analysis-affecting domain rules
//!     are added or deleted.
//!
//! # Usage
//! ```ignore
//! let inval = Arc::new(RuntimeInvalidation::new());
//! // On mutation:
//! inval.graph_generation.fetch_add(1, Ordering::Relaxed);
//! // On check:
//! let current = inval.graph_generation.load(Ordering::Relaxed);
//! ```

use std::sync::atomic::AtomicU64;

/// Centralized invalidation counters for runtime components.
///
/// Each counter is a monotonically-increasing generation number.
/// Components bump their counter when they modify state; consumers
/// compare their cached generation to decide whether to refresh.
pub(crate) struct RuntimeInvalidation {
    /// Bumped on graph-affecting mutations: overlay annotations, index completion,
    /// lazy structural writes.
    pub(crate) graph_generation: AtomicU64,
    /// Bumped on overlay mutations: fp_dispatches add/delete.
    pub(crate) overlay_generation: AtomicU64,
    /// Bumped on domain_rules / fp_dispatches mutations.
    /// Future consumer: BranchDiffEngine & FieldLifecycleEngine cache invalidation.
    /// Currently write-only (no readers yet).
    pub(crate) analysis_config_generation: AtomicU64,
}

impl RuntimeInvalidation {
    /// All counters start at 1, not 0.
    ///
    /// This ensures a consumer whose cached generation is 0 (uninitialized)
    /// will always be treated as stale by `>` comparisons (e.g.
    /// `current_gen > cached_gen`).  A 0-start would require consumers to
    /// special-case "never loaded" vs "loaded gen 0".
    pub(crate) fn new() -> Self {
        Self {
            graph_generation: AtomicU64::new(1),
            overlay_generation: AtomicU64::new(1),
            analysis_config_generation: AtomicU64::new(1),
        }
    }
}
