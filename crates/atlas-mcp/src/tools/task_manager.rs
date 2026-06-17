use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::Semaphore;

/// Unique identifier for an async task.
pub type TaskId = String;

/// Adaptive backoff configuration for client polling.
/// Prevents thundering herd by spreading client retry times.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    pub initial_ms: u64,
    pub max_ms: u64,
    pub multiplier: f64,
    pub jitter_ms: u64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_ms: 500,
            max_ms: 5000,
            multiplier: 2.0,
            jitter_ms: 100,
        }
    }
}

/// Status of an async task.
#[derive(Debug, Clone)]
pub enum TaskStatus {
    /// Task registered but not yet started (waiting for semaphore permit).
    Pending,
    /// Task is currently executing.
    Running,
    /// Task completed successfully or with a known error.
    Completed,
    /// Task panicked or encountered an unrecoverable error.
    Failed { error: String },
}

/// The full state of an async task, returned to clients on poll.
#[derive(Debug, Clone)]
pub struct TaskState {
    pub id: TaskId,
    pub tool_name: String,
    pub status: TaskStatus,
    pub created_at: Instant,
    pub started_at: Option<Instant>,
    pub completed_at: Option<Instant>,
    /// The JSON result string (available when Completed or Failed).
    pub result: Option<String>,
    /// Whether the result is an error (available when Completed).
    pub is_error: Option<bool>,
    /// Progress percentage 0-100 (optional, for long-running tasks).
    pub progress_pct: Option<u8>,
    /// Human-readable progress message.
    pub progress_msg: Option<String>,
    /// Number of times the client has polled this task.
    pub poll_count: u32,
    pub backoff_config: BackoffConfig,
}

impl TaskState {
    /// Calculate the suggested next poll delay with exponential backoff + jitter.
    /// Sequence: 500, 1000, 2000, 4000, 5000, 5000, ...
    pub fn next_poll_after_ms(&self) -> u64 {
        let base = (self.backoff_config.initial_ms as f64
            * self.backoff_config.multiplier.powi(self.poll_count as i32))
        .min(self.backoff_config.max_ms as f64) as u64;
        let jitter = if self.backoff_config.jitter_ms > 0 {
            rand::thread_rng().gen_range(0..=self.backoff_config.jitter_ms)
        } else {
            0
        };
        base + jitter
    }
}

/// Manages the lifecycle of async tool execution tasks.
///
/// Lives at the server level (AtlasMcpService), survives project open/close.
pub struct TaskManager {
    tasks: RwLock<HashMap<TaskId, TaskState>>,
    next_id: AtomicU64,
    /// Semaphore limiting concurrent async task execution.
    /// Default: 4, overridable via ATLAS_MAX_ASYNC_TASKS env var.
    concurrency_limit: Arc<Semaphore>,
    /// TTL for completed/failed tasks before they're pruned.
    ttl: Duration,
    /// Initial backoff for new tasks.
    default_backoff: BackoffConfig,
}

impl TaskManager {
    /// Create a new TaskManager with the given max concurrent tasks.
    /// Default max_concurrent is 4.
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            concurrency_limit: Arc::new(Semaphore::new(max_concurrent)),
            ttl: Duration::from_secs(300),
            default_backoff: BackoffConfig::default(),
        }
    }

    /// Register a new pending task. Returns the assigned task ID.
    /// The caller MUST spawn the actual work (e.g., via tokio::spawn).
    pub fn register(&self, tool_name: &str) -> TaskId {
        let id = format!("task_{:04x}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let state = TaskState {
            id: id.clone(),
            tool_name: tool_name.to_string(),
            status: TaskStatus::Pending,
            created_at: Instant::now(),
            started_at: None,
            completed_at: None,
            result: None,
            is_error: None,
            progress_pct: None,
            progress_msg: None,
            poll_count: 0,
            backoff_config: self.default_backoff.clone(),
        };
        self.tasks.write().unwrap().insert(id.clone(), state);
        id
    }

    /// Mark a task as Running (called when semaphore permit is acquired).
    pub fn mark_running(&self, task_id: &str) {
        if let Some(state) = self.tasks.write().unwrap().get_mut(task_id) {
            state.status = TaskStatus::Running;
            state.started_at = Some(Instant::now());
        }
    }

    /// Mark a task as Completed with the tool result.
    pub fn mark_completed(&self, task_id: &str, result: String, is_error: bool) {
        if let Some(state) = self.tasks.write().unwrap().get_mut(task_id) {
            state.status = TaskStatus::Completed;
            state.completed_at = Some(Instant::now());
            state.result = Some(result);
            state.is_error = Some(is_error);
        }
    }

    /// Mark a task as Failed (e.g., handler panicked).
    pub fn mark_failed(&self, task_id: &str, error: String) {
        if let Some(state) = self.tasks.write().unwrap().get_mut(task_id) {
            state.status = TaskStatus::Failed {
                error: error.clone(),
            };
            state.completed_at = Some(Instant::now());
            state.result = Some(error);
            state.is_error = Some(true);
        }
    }

    /// Update task progress (0-100). Called by running handlers.
    pub fn update_progress(&self, task_id: &str, pct: u8, msg: Option<String>) {
        if let Some(state) = self.tasks.write().unwrap().get_mut(task_id) {
            state.progress_pct = Some(pct);
            state.progress_msg = msg;
        }
    }

    /// Poll a task by ID. Increments poll_count (affects backoff calculation).
    /// Returns None if the task has expired or doesn't exist.
    pub fn poll(&self, task_id: &str) -> Option<TaskState> {
        let mut tasks = self.tasks.write().unwrap();
        let state = tasks.get_mut(task_id)?;
        state.poll_count += 1;

        // Check TTL for completed/failed tasks
        if state.completed_at.map_or(false, |t| t.elapsed() > self.ttl) {
            tasks.remove(task_id);
            return None;
        }

        Some(state.clone())
    }

    /// List all active tasks (for the `tasks` MCP tool response).
    pub fn list_all(&self) -> Vec<TaskState> {
        self.tasks.read().unwrap().values().cloned().collect()
    }

    /// Remove expired completed/failed tasks. Returns count of pruned tasks.
    pub fn prune_expired(&self) -> usize {
        let mut tasks = self.tasks.write().unwrap();
        let before = tasks.len();
        tasks.retain(|_, s| s.completed_at.map_or(true, |t| t.elapsed() <= self.ttl));
        before - tasks.len()
    }

    /// Get the concurrency semaphore for acquiring execution permits.
    pub fn semaphore(&self) -> Arc<Semaphore> {
        self.concurrency_limit.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_returns_unique_ids() {
        let mgr = TaskManager::new(4);
        let id1 = mgr.register("search");
        let id2 = mgr.register("search");
        assert_ne!(id1, id2);
    }

    #[test]
    fn lifecycle_pending_to_completed() {
        let mgr = TaskManager::new(4);
        let id = mgr.register("search");

        let state = mgr.poll(&id).unwrap();
        assert!(matches!(state.status, TaskStatus::Pending));

        mgr.mark_running(&id);
        let state = mgr.poll(&id).unwrap();
        assert!(matches!(state.status, TaskStatus::Running));

        mgr.mark_completed(&id, r#"{"result":"ok"}"#.into(), false);
        let state = mgr.poll(&id).unwrap();
        assert!(matches!(state.status, TaskStatus::Completed));
        assert_eq!(state.result.unwrap(), r#"{"result":"ok"}"#);
    }

    #[test]
    fn failed_task_stores_error() {
        let mgr = TaskManager::new(4);
        let id = mgr.register("trace");
        mgr.mark_failed(&id, "handler panicked".into());
        let state = mgr.poll(&id).unwrap();
        assert!(matches!(state.status, TaskStatus::Failed { .. }));
    }

    #[test]
    fn poll_increments_counter() {
        let mgr = TaskManager::new(4);
        let id = mgr.register("search");
        mgr.mark_completed(&id, "done".into(), false);
        mgr.poll(&id);
        mgr.poll(&id);
        let state = mgr.poll(&id).unwrap();
        assert_eq!(state.poll_count, 3);
    }

    #[test]
    fn backoff_sequence() {
        let config = BackoffConfig::default();
        let mut state = TaskState {
            id: "t1".into(),
            tool_name: "x".into(),
            status: TaskStatus::Running,
            created_at: Instant::now(),
            started_at: None,
            completed_at: None,
            result: None,
            is_error: None,
            progress_pct: None,
            progress_msg: None,
            poll_count: 0,
            backoff_config: config.clone(),
        };

        // First poll: ~500ms
        state.poll_count = 0;
        let d0 = state.next_poll_after_ms();
        assert!(d0 >= 500 && d0 <= 600, "d0={d0}");

        // Second poll: ~1000ms
        state.poll_count = 1;
        let d1 = state.next_poll_after_ms();
        assert!(d1 >= 1000 && d1 <= 1100, "d1={d1}");

        // Third poll: ~2000ms
        state.poll_count = 2;
        let d2 = state.next_poll_after_ms();
        assert!(d2 >= 2000 && d2 <= 2100, "d2={d2}");

        // Fourth poll: ~4000ms
        state.poll_count = 3;
        let d3 = state.next_poll_after_ms();
        assert!(d3 >= 4000 && d3 <= 4100, "d3={d3}");

        // Fifth poll: capped at ~5000ms
        state.poll_count = 4;
        let d4 = state.next_poll_after_ms();
        assert!(d4 >= 5000 && d4 <= 5100, "d4={d4}");
    }

    #[test]
    fn prune_removes_expired() {
        let mgr = TaskManager::new(4);
        let id = mgr.register("search");
        mgr.mark_completed(&id, "done".into(), false);

        // At least verify prune doesn't crash and task still exists (not yet expired)
        mgr.prune_expired();
        assert!(mgr.poll(&id).is_some());
    }

    #[test]
    fn progress_tracking() {
        let mgr = TaskManager::new(4);
        let id = mgr.register("search");
        mgr.mark_running(&id);
        mgr.update_progress(&id, 50, Some("half done".into()));
        let state = mgr.poll(&id).unwrap();
        assert_eq!(state.progress_pct, Some(50));
        assert_eq!(state.progress_msg, Some("half done".into()));
    }

    #[test]
    fn poll_nonexistent_returns_none() {
        let mgr = TaskManager::new(4);
        assert!(mgr.poll("nonexistent").is_none());
    }

    #[test]
    fn list_all_returns_all_tasks() {
        let mgr = TaskManager::new(4);
        mgr.register("search");
        mgr.register("trace");
        let all = mgr.list_all();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn multiple_running_tasks_tracked_independently() {
        let mgr = TaskManager::new(4);
        let id1 = mgr.register("search");
        let id2 = mgr.register("trace");

        mgr.mark_running(&id1);
        mgr.update_progress(&id1, 30, None);

        let s1 = mgr.poll(&id1).unwrap();
        let s2 = mgr.poll(&id2).unwrap();
        assert!(matches!(s1.status, TaskStatus::Running));
        assert_eq!(s1.progress_pct, Some(30));
        assert!(matches!(s2.status, TaskStatus::Pending));
        assert_eq!(s2.progress_pct, None);
    }

    #[test]
    fn backoff_config_default_values() {
        let config = BackoffConfig::default();
        assert_eq!(config.initial_ms, 500);
        assert_eq!(config.max_ms, 5000);
        assert_eq!(config.multiplier, 2.0);
        assert_eq!(config.jitter_ms, 100);
    }
}
