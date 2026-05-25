//! Wait-for tool: server-side blocking poll for background task completion.
//!
//! Unlike `task_status` (one-shot poll), `wait_for_task` blocks until the
//! background task completes or the timeout expires.  This provides a simple
//! "fire and forget" pattern for clients that don't want to implement polling.

use std::time::{Duration, Instant};

use super::ToolRouter;
use super::get_str;

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_wait_for_task(&mut self, args: &serde_json::Value) -> (String, bool) {
        let task_id = get_str(args, "task_id");
        if task_id.is_empty() {
            return ("Missing task_id parameter".to_string(), true);
        }

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(300) as u64; // cap at 5 minutes
        let poll_interval_secs = args
            .get("poll_interval_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(2)
            .clamp(1, 10) as u64;

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let poll_duration = Duration::from_secs(poll_interval_secs);

        loop {
            let info = match self.task_manager.get_task(task_id) {
                Some(info) => info,
                None => return (format!("Task not found: {}", task_id), true),
            };

            let status_str = match info.status {
                crate::task_manager::TaskStatus::Running => "running",
                crate::task_manager::TaskStatus::Completed => "completed",
                crate::task_manager::TaskStatus::Failed => "failed",
            };

            if status_str != "running" {
                // Task finished — return final result
                let mut response = json!({
                    "task_id": info.task_id,
                    "tool_name": info.tool_name,
                    "method": info.method,
                    "status": status_str,
                    "progress": info.progress,
                    "progress_message": info.progress_message,
                    "elapsed_secs": info.elapsed_secs(),
                });
                if let Some(ref result) = info.result {
                    response["result"] = result.clone();
                }
                if let Some(ref error) = info.error {
                    response["error"] = serde_json::Value::String(error.clone());
                }
                if status_str == "completed" && info.method == "open_project" {
                    if let Some(project) = self.activate_pending_project_for_task(&info.task_id) {
                        response["activation"] = serde_json::Value::String("activated".into());
                        response["activated_project"] = serde_json::Value::String(project);
                    } else {
                        response["activation"] =
                            serde_json::Value::String("already_activated".into());
                    }
                }
                return (
                    serde_json::to_string_pretty(&response).unwrap_or_else(|e| e.to_string()),
                    false,
                );
            }

            // Check timeout
            if timeout_secs == 0 || Instant::now() >= deadline {
                return (serde_json::to_string_pretty(&json!({
                    "task_id": info.task_id,
                    "tool_name": info.tool_name,
                    "method": info.method,
                    "status": "running",
                    "progress": info.progress,
                    "progress_message": info.progress_message,
                    "elapsed_secs": info.elapsed_secs(),
                    "note": "Task still running. Increase timeout_secs or continue polling with task_status."
                })).unwrap_or_else(|e| e.to_string()), false);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            let sleep_for = std::cmp::min(poll_duration, remaining);
            if sleep_for.is_zero() {
                return (
                    serde_json::to_string_pretty(&json!({
                        "task_id": info.task_id, "status": "running",
                        "tool_name": info.tool_name, "method": info.method,
                        "progress": info.progress, "note": "Timeout reached."
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    false,
                );
            }
            std::thread::sleep(sleep_for);
        }
    }
}
