//! Background task manager for long-running MCP tool operations.
//!
//! Enables `background: true` on long-running tools — the tool returns
//! immediately with a `task_id`, and the client can poll `task_status` or call
//! `wait_for_task` for completion. This avoids MCP timeouts on clients that do
//! not support `notify_progress`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Completed/failed task retention window.
pub const CLEANUP_AFTER_SECS: u64 = 300;

/// Minimum progress delta before an in-flight update is recorded.
const PROGRESS_UPDATE_THRESHOLD: f64 = 5.0;

/// Status of a background task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
}

/// Information about a single background task.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub task_id: String,
    pub tool_name: String,
    pub method: String,
    pub status: TaskStatus,
    /// Percent complete in the inclusive range 0.0..=100.0.
    pub progress: Option<f64>,
    pub progress_message: Option<String>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub created_at: Instant,
    pub completed_at: Option<Instant>,
}

impl TaskInfo {
    pub fn elapsed_secs(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }
}

#[derive(Default)]
struct TaskManagerInner {
    tasks: HashMap<String, TaskInfo>,
    next_id: u64,
}

/// Thread-safe manager for background tasks.
pub struct TaskManager {
    inner: Mutex<TaskManagerInner>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TaskManagerInner::default()),
        }
    }

    /// Register a new running task with an automatically generated `task-xxxxx`
    /// id and return its task_id.
    pub fn create_task(&self, tool_name: &str, method: &str) -> String {
        let mut inner = self.inner.lock().unwrap();
        prune_old_locked(&mut inner, Duration::from_secs(CLEANUP_AFTER_SECS));

        let task_id = next_auto_task_id(&mut inner);
        let info = new_task_info(&task_id, tool_name, method);
        inner.tasks.insert(task_id.clone(), info);
        task_id
    }

    /// Register a new running task with a caller-provided id.
    ///
    /// Returns `false` if the id already exists after normal pruning. Custom ids
    /// are reserved for stable task domains such as `analysis:{short_hash}`;
    /// ordinary background tools should use [`Self::create_task`].
    pub fn create_task_with_id(&self, task_id: &str, tool_name: &str, method: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        prune_old_locked(&mut inner, Duration::from_secs(CLEANUP_AFTER_SECS));

        if inner.tasks.contains_key(task_id) {
            return false;
        }

        let info = new_task_info(task_id, tool_name, method);
        inner.tasks.insert(task_id.to_string(), info);
        true
    }

    /// Update task progress.
    ///
    /// Progress is clamped to 0..=100 and throttled to at least 5 percentage
    /// points between stored updates, except the initial and final (100%)
    /// updates which are always recorded.
    pub fn update_progress(&self, task_id: &str, percent: f64, message: &str) {
        if !percent.is_finite() {
            return;
        }
        let percent = percent.clamp(0.0, 100.0);

        if let Some(info) = self.inner.lock().unwrap().tasks.get_mut(task_id) {
            let should_update = match info.progress {
                None => true,
                Some(_) if percent >= 100.0 => true,
                Some(prev) => (percent - prev).abs() >= PROGRESS_UPDATE_THRESHOLD,
            };
            if should_update {
                info.progress = Some(percent);
                info.progress_message = Some(message.to_string());
            }
        }
    }

    pub fn complete_task(&self, task_id: &str, result: serde_json::Value) {
        if let Some(info) = self.inner.lock().unwrap().tasks.get_mut(task_id) {
            info.status = TaskStatus::Completed;
            info.progress = Some(100.0);
            info.result = Some(result);
            info.completed_at = Some(Instant::now());
        }
    }

    pub fn fail_task(&self, task_id: &str, error: &str) {
        if let Some(info) = self.inner.lock().unwrap().tasks.get_mut(task_id) {
            info.status = TaskStatus::Failed;
            info.error = Some(error.to_string());
            info.completed_at = Some(Instant::now());
        }
    }

    pub fn get_task(&self, task_id: &str) -> Option<TaskInfo> {
        let mut inner = self.inner.lock().unwrap();
        prune_old_locked(&mut inner, Duration::from_secs(CLEANUP_AFTER_SECS));
        inner.tasks.get(task_id).cloned()
    }

    /// Prune completed/failed tasks using the default retention window.
    pub fn prune_old(&self) {
        self.prune_old_tasks(Duration::from_secs(CLEANUP_AFTER_SECS));
    }

    /// Prune completed/failed tasks older than `max_age`.
    ///
    /// Running tasks are always retained.
    pub fn prune_old_tasks(&self, max_age: Duration) {
        let mut inner = self.inner.lock().unwrap();
        prune_old_locked(&mut inner, max_age);
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

fn new_task_info(task_id: &str, tool_name: &str, method: &str) -> TaskInfo {
    TaskInfo {
        task_id: task_id.to_string(),
        tool_name: tool_name.to_string(),
        method: method.to_string(),
        status: TaskStatus::Running,
        progress: None,
        progress_message: None,
        result: None,
        error: None,
        created_at: Instant::now(),
        completed_at: None,
    }
}

fn next_auto_task_id(inner: &mut TaskManagerInner) -> String {
    loop {
        let task_id = format!("task-{:05x}", inner.next_id);
        inner.next_id = inner.next_id.saturating_add(1);
        if !inner.tasks.contains_key(&task_id) {
            return task_id;
        }
    }
}

fn prune_old_locked(inner: &mut TaskManagerInner, max_age: Duration) {
    inner.tasks.retain(|_, info| {
        if info.status == TaskStatus::Running {
            return true;
        }
        info.completed_at
            .map(|at| at.elapsed() < max_age)
            .unwrap_or(true)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_ids_are_sequential_hex() {
        let tm = TaskManager::new();
        let first = tm.create_task("search", "search");
        let second = tm.create_task("search", "search");

        assert_eq!(first, "task-00000");
        assert_eq!(second, "task-00001");
        assert_eq!(tm.get_task(&first).unwrap().method, "search");
    }

    #[test]
    fn custom_ids_are_unique() {
        let tm = TaskManager::new();

        assert!(tm.create_task_with_id("analysis:abc123", "analysis", "analyze_file"));
        assert!(!tm.create_task_with_id("analysis:abc123", "analysis", "analyze_file"));
        assert_eq!(
            tm.get_task("analysis:abc123").unwrap().tool_name,
            "analysis"
        );
    }

    #[test]
    fn auto_id_skips_existing_custom_id() {
        let tm = TaskManager::new();

        assert!(tm.create_task_with_id("task-00000", "test", "custom"));
        assert_eq!(tm.create_task("search", "search"), "task-00001");
    }

    #[test]
    fn progress_updates_are_clamped_and_throttled() {
        let tm = TaskManager::new();
        let task_id = tm.create_task("search", "search");

        tm.update_progress(&task_id, 1.0, "first");
        tm.update_progress(&task_id, 3.0, "below threshold");
        let info = tm.get_task(&task_id).unwrap();
        assert_eq!(info.progress, Some(1.0));
        assert_eq!(info.progress_message.as_deref(), Some("first"));

        tm.update_progress(&task_id, 6.0, "threshold reached");
        assert_eq!(tm.get_task(&task_id).unwrap().progress, Some(6.0));

        tm.update_progress(&task_id, 150.0, "done");
        assert_eq!(tm.get_task(&task_id).unwrap().progress, Some(100.0));
    }

    #[test]
    fn pruning_removes_finished_tasks_but_keeps_running_tasks() {
        let tm = TaskManager::new();
        let done = tm.create_task("search", "search");
        let running = tm.create_task("search", "search");

        tm.complete_task(&done, json!({"ok": true}));
        tm.prune_old_tasks(Duration::ZERO);

        assert!(tm.get_task(&done).is_none());
        assert!(tm.get_task(&running).is_some());
    }
}
