//! Background task manager for long-running MCP tool operations.
//!
//! Enables `background: true` on tools like `index` and `search` —
//! the tool returns immediately with a `task_id`, and the client
//! can poll `task_status` for completion.  This avoids MCP timeouts
//! on clients that do not support `notify_progress`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
    pub status: TaskStatus,
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

/// Thread-safe manager for background tasks.
pub struct TaskManager {
    tasks: Mutex<HashMap<String, TaskInfo>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self { tasks: Mutex::new(HashMap::new()) }
    }

    /// Register a new running task and return its task_id.
    pub fn create_task(&self, tool_name: &str) -> String {
        let task_id = gen_task_id(tool_name);
        let info = TaskInfo {
            task_id: task_id.clone(),
            tool_name: tool_name.to_string(),
            status: TaskStatus::Running,
            progress: None,
            progress_message: None,
            result: None,
            error: None,
            created_at: Instant::now(),
            completed_at: None,
        };
        self.tasks.lock().unwrap().insert(task_id.clone(), info);
        task_id
    }

    pub fn update_progress(&self, task_id: &str, percent: f64, message: &str) {
        if let Some(info) = self.tasks.lock().unwrap().get_mut(task_id) {
            info.progress = Some(percent);
            info.progress_message = Some(message.to_string());
        }
    }

    pub fn complete_task(&self, task_id: &str, result: serde_json::Value) {
        if let Some(info) = self.tasks.lock().unwrap().get_mut(task_id) {
            info.status = TaskStatus::Completed;
            info.progress = Some(100.0);
            info.result = Some(result);
            info.completed_at = Some(Instant::now());
        }
    }

    pub fn fail_task(&self, task_id: &str, error: &str) {
        if let Some(info) = self.tasks.lock().unwrap().get_mut(task_id) {
            info.status = TaskStatus::Failed;
            info.error = Some(error.to_string());
            info.completed_at = Some(Instant::now());
        }
    }

    pub fn get_task(&self, task_id: &str) -> Option<TaskInfo> {
        self.tasks.lock().unwrap().get(task_id).cloned()
    }

    pub fn prune_old_tasks(&self, max_age: Duration) {
        self.tasks.lock().unwrap().retain(|_, info| {
            if info.status == TaskStatus::Running { return true; }
            info.completed_at.map(|at| at.elapsed() < max_age).unwrap_or(true)
        });
    }
}

fn gen_task_id(tool_name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let hash = blake3::hash(format!("{}-{}", tool_name, nanos).as_bytes());
    hex::encode(&hash.as_bytes()[..4])
}
