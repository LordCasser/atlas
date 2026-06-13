use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::Instant;

use atlas_engine::InvestigationFocus;

use crate::task_manager::TaskManager;
use crate::tools::PendingProjectActivation;
use crate::tools::query_snapshot::{QUERY_SNAPSHOT_TTL_SECS, InvestigationState, QuerySnapshot};

/// Background task management and async query state.
///
/// Owns the TaskManager for async operations (index, background project open),
/// query snapshots for lazy responses, and investigation state for
/// lazy job prioritization.
pub struct JobRuntime {
    pub task_manager: Arc<TaskManager>,
    pub investigation_state: InvestigationState,
    pub query_snapshots: Mutex<HashMap<String, QuerySnapshot>>,
    /// Project activations prepared by background `open_project` tasks.
    pub pending_project_activations: Arc<Mutex<HashMap<String, PendingProjectActivation>>>,
    /// Per-store prewarm guard: at most one background dataflow prewarm
    /// thread per store, shared across all concurrent MCP requests.
    /// Reserved for future dataflow prewarm orchestration.
    #[allow(dead_code)]
    pub prewarm_running: Arc<AtomicBool>,
}

impl JobRuntime {
    pub fn new(task_manager: Arc<TaskManager>) -> Self {
        Self {
            task_manager,
            investigation_state: InvestigationState::default(),
            query_snapshots: Mutex::new(HashMap::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
            prewarm_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Remove query snapshots older than TTL.
    pub fn prune_expired_snapshots(&self) {
        let cutoff = Instant::now()
            - std::time::Duration::from_secs(QUERY_SNAPSHOT_TTL_SECS);
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

    fn create_test_job_runtime() -> JobRuntime {
        let task_manager = Arc::new(TaskManager::new());
        JobRuntime::new(task_manager)
    }

    #[test]
    fn prewarm_running_starts_false() {
        let jr = create_test_job_runtime();
        // Prove the prewarm_running flag is wired correctly — reserved for
        // future dataflow prewarm orchestration.
        assert!(!jr.prewarm_running.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn investigation_state_starts_default() {
        let jr = create_test_job_runtime();
        assert!(jr.investigation_state.active_investigation.is_none());
    }
}
