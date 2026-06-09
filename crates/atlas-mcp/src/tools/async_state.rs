//! Background task and async operation state management.
//!
//! Extracted from [`super::ToolRouter`] to reduce the God-object footprint.
//! Owns the task manager, pending project activations, query snapshots,
//! and prewarm coordination.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::Instant;

use crate::tools::PendingProjectActivation;
use crate::tools::query_snapshot::{QUERY_SNAPSHOT_TTL_SECS, QuerySnapshot};

/// Background task and async operation state.
pub(crate) struct AsyncState {
    /// Background task manager for `background: true` mode.
    pub(crate) task_manager: Arc<crate::task_manager::TaskManager>,
    /// Project activations prepared by background `open_project` tasks.
    pub(crate) pending_project_activations: Arc<Mutex<HashMap<String, PendingProjectActivation>>>,
    /// In-memory query snapshots for `atlas_resume`.
    pub(crate) query_snapshots: Mutex<HashMap<String, QuerySnapshot>>,
    /// Per-store prewarm guard: at most one background dataflow prewarm
    /// thread per store, shared across all concurrent MCP requests.
    pub(crate) prewarm_running: Arc<AtomicBool>,
}

impl AsyncState {
    /// Remove query snapshots older than TTL.
    pub(crate) fn prune_expired_snapshots(&self) {
        let cutoff = Instant::now()
            - std::time::Duration::from_secs(QUERY_SNAPSHOT_TTL_SECS);
        self.query_snapshots
            .lock()
            .unwrap()
            .retain(|_, s| s.created_at > cutoff);
    }
}
