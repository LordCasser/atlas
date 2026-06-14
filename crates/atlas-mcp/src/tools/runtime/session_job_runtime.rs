//! Session-level job runtime — lives for the entire MCP session.
//!
//! Holds session-scoped resources that persist across project switches:
//! - `TaskManager` — background task tracking
//! - `pending_project_activations` — stores prepared project data waiting
//!   for the client to confirm activation via `task_status`/`wait_for_task`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

use crate::task_manager::TaskManager;
use crate::tools::PendingProjectActivation;

pub(crate) struct SessionJobRuntime {
    pub(crate) task_manager: Arc<TaskManager>,
    pub(crate) pending_project_activations: Arc<Mutex<HashMap<String, PendingProjectActivation>>>,
    /// Per-store prewarm guard: at most one background dataflow prewarm
    /// thread per store, shared across all concurrent MCP requests.
    /// Reserved for future dataflow prewarm orchestration.
    #[allow(dead_code)]
    pub(crate) prewarm_running: Arc<AtomicBool>,
}

impl SessionJobRuntime {
    pub(crate) fn new() -> Self {
        Self {
            task_manager: Arc::new(TaskManager::new()),
            pending_project_activations: Arc::new(Mutex::new(HashMap::new())),
            prewarm_running: Arc::new(AtomicBool::new(false)),
        }
    }
}
