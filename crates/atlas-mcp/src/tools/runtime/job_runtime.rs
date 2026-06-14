//! Project-level job runtime — investigation state and query snapshots.
//!
//! The MCP public contract no longer exposes task-manager background jobs;
//! this runtime only owns per-project query and investigation state.
//!
//! # Responsibilities
//! - InvestigationState: per-project lazy extraction prioritization
//! - Query snapshots: store and retrieve query results for resume_query
//!
//! # Usage pattern
//! ```ignore
//! self.active.job_runtime.store_snapshot(snapshot);
//! self.active.job_runtime.update_investigation(focus);
//! ```
//!
//! # Dependencies
//! - `atlas_engine::InvestigationFocus`

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use atlas_engine::InvestigationFocus;

use crate::tools::query_snapshot::{InvestigationState, QUERY_SNAPSHOT_TTL_SECS, QuerySnapshot};

/// Per-project query state and investigation tracking.
///
/// Owns query snapshots for lazy responses and investigation state for
/// lazy job prioritization.
pub struct JobRuntime {
    pub investigation_state: InvestigationState,
    pub query_snapshots: Mutex<HashMap<String, QuerySnapshot>>,
}

impl JobRuntime {
    pub fn new() -> Self {
        Self {
            investigation_state: InvestigationState::default(),
            query_snapshots: Mutex::new(HashMap::new()),
        }
    }

    /// Remove query snapshots older than TTL.
    pub fn prune_expired_snapshots(&self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(QUERY_SNAPSHOT_TTL_SECS);
        self.query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, s| s.created_at > cutoff);
    }

    /// Store a query snapshot, pruning expired entries first.
    ///
    /// Recovers from a poisoned lock (e.g. after a panic in another handler)
    /// rather than panicking — consistent with `AtlasMcpService::lock_router()`.
    pub fn store_snapshot(&self, snapshot: QuerySnapshot) {
        self.prune_expired_snapshots();
        self.query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(snapshot.query_id.clone(), snapshot);
    }

    /// Update or create investigation based on a tool call focus.
    pub fn update_investigation(&mut self, focus: InvestigationFocus) {
        self.investigation_state.update(focus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigation_state_starts_default() {
        let jr = JobRuntime::new();
        assert!(jr.investigation_state.active_investigation.is_none());
    }

    #[test]
    fn query_snapshots_starts_empty() {
        let jr = JobRuntime::new();
        assert!(jr.query_snapshots.lock().unwrap().is_empty());
    }
}
