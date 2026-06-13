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
    /// Bumped on analysis-affecting mutations: domain_rules add/delete.
    pub(crate) analysis_config_generation: AtomicU64,
}

impl RuntimeInvalidation {
    pub(crate) fn new() -> Self {
        Self {
            graph_generation: AtomicU64::new(1),
            overlay_generation: AtomicU64::new(1),
            analysis_config_generation: AtomicU64::new(1),
        }
    }
}
