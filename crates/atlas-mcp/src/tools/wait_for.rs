//! Wait-for tool: server-side async poll for background task completion.
//!
//! Unlike `task_status` (one-shot poll), `wait_for_task` polls until the
//! background task completes or the timeout expires.  This provides a simple
//! "fire and forget" pattern for clients that don't want to implement polling.
//!
//! This is an async function so the single-threaded tokio runtime is not
//! blocked while waiting — progress notifications and other tasks can
//! continue to make progress during the poll intervals.

use std::time::Duration;

use super::get_str;

use serde_json::json;

/// Result returned by the wait-for poll loop.
///
/// `task_is_project_completed` signals whether the caller should attempt
/// `activate_pending_project_for_task` (requires `&mut ToolRouter`).
pub(crate) struct WaitForResult {
    pub json_text: String,
    pub is_error: bool,
    pub task_is_project_completed: bool,
}

/// Async poll loop for `wait_for_task`.
///
/// Uses `tokio::time::sleep` so that the single-threaded runtime can make
/// progress on other work (e.g. progress notifications) between polls.
pub(crate) async fn handle_wait_for_task(
    task_manager: &crate::task_manager::TaskManager,
    args: &serde_json::Value,
) -> WaitForResult {
    let task_id = get_str(args, "task_id");
    if task_id.is_empty() {
        return WaitForResult {
            json_text: serde_json::to_string_pretty(&json!({
                "error": "Missing task_id parameter"
            }))
            .unwrap_or_else(|e| e.to_string()),
            is_error: true,
            task_is_project_completed: false,
        };
    }

    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .min(300); // cap at 5 minutes
    let poll_interval_secs = args
        .get("poll_interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .clamp(1, 10);

    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let poll_duration = Duration::from_secs(poll_interval_secs);

    loop {
        let info = match task_manager.get_task(task_id) {
            Some(info) => info,
            None => {
                return WaitForResult {
                    json_text: format!("Task not found: {task_id}"),
                    is_error: true,
                    task_is_project_completed: false,
                };
            }
        };

        let status_str = match info.status {
            crate::task_manager::TaskStatus::Running => "running",
            crate::task_manager::TaskStatus::Completed => "completed",
            crate::task_manager::TaskStatus::Failed => "failed",
        };

        if status_str != "running" {
            // Task finished — return final result (project activation handled
            // by the caller in lib.rs, which can re-acquire &mut ToolRouter).
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
            let is_project_completed =
                status_str == "completed" && info.method == "project";
            return WaitForResult {
                json_text: serde_json::to_string_pretty(&response)
                    .unwrap_or_else(|e| e.to_string()),
                is_error: false,
                task_is_project_completed: is_project_completed,
            };
        }

        // Check timeout
        if timeout_secs == 0 || tokio::time::Instant::now() >= deadline {
            return WaitForResult {
                json_text: serde_json::to_string_pretty(&json!({
                    "task_id": info.task_id,
                    "tool_name": info.tool_name,
                    "method": info.method,
                    "status": "running",
                    "progress": info.progress,
                    "progress_message": info.progress_message,
                    "elapsed_secs": info.elapsed_secs(),
                    "note": "Task still running. Increase timeout_secs or continue polling with task_status."
                }))
                .unwrap_or_else(|e| e.to_string()),
                is_error: false,
                task_is_project_completed: false,
            };
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let sleep_for = std::cmp::min(poll_duration, remaining);
        if sleep_for.is_zero() {
            return WaitForResult {
                json_text: serde_json::to_string_pretty(&json!({
                    "task_id": info.task_id, "status": "running",
                    "tool_name": info.tool_name, "method": info.method,
                    "progress": info.progress, "note": "Timeout reached."
                }))
                .unwrap_or_else(|e| e.to_string()),
                is_error: false,
                task_is_project_completed: false,
            };
        }
        tokio::time::sleep(sleep_for).await;
    }
}

/// Synchronous version of [`handle_wait_for_task`] for use in tests and
/// embedded callers that invoke `ToolRouter::call_tool` directly.
pub(crate) fn handle_wait_for_task_sync(
    task_manager: &crate::task_manager::TaskManager,
    args: &serde_json::Value,
) -> WaitForResult {
    let task_id = get_str(args, "task_id");
    if task_id.is_empty() {
        return WaitForResult {
            json_text: serde_json::to_string_pretty(&json!({
                "error": "Missing task_id parameter"
            }))
            .unwrap_or_default(),
            is_error: true,
            task_is_project_completed: false,
        };
    }

    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .min(300);
    let poll_interval_secs = args
        .get("poll_interval_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .max(1)
        .min(10);

    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match task_manager.get_task(task_id) {
            Some(info) => match info.status {
                crate::task_manager::TaskStatus::Completed => {
                    let result = info
                        .result
                        .clone()
                        .unwrap_or_else(|| json!({"status": "completed"}));
                    let mut response = serde_json::from_str::<serde_json::Value>(
                        &serde_json::to_string(&result).unwrap_or_default(),
                    )
                    .unwrap_or(json!({"status": "completed"}));
                    response["task_id"] = json!(task_id);
                    response["status"] = json!("completed");
                    response["elapsed_secs"] = json!(info.elapsed_secs());
                    return WaitForResult {
                        json_text: serde_json::to_string_pretty(&response).unwrap_or_default(),
                        is_error: false,
                        task_is_project_completed: info.method == "project",
                    };
                }
                crate::task_manager::TaskStatus::Failed => {
                    let msg = info.error.clone().unwrap_or_else(|| "unknown error".to_string());
                    return WaitForResult {
                        json_text: serde_json::to_string_pretty(&json!({
                            "task_id": task_id,
                            "status": "failed",
                            "error": msg,
                            "elapsed_secs": info.elapsed_secs(),
                        }))
                        .unwrap_or_default(),
                        is_error: true,
                        task_is_project_completed: false,
                    };
                }
                crate::task_manager::TaskStatus::Running => {
                    // Keep polling
                }
            },
            None => {
                return WaitForResult {
                    json_text: serde_json::to_string_pretty(&json!({
                        "error": format!("Task not found: {task_id}")
                    }))
                    .unwrap_or_default(),
                    is_error: true,
                    task_is_project_completed: false,
                };
            }
        }

        if std::time::Instant::now() >= deadline {
            match task_manager.get_task(task_id) {
                Some(info) => {
                    let status_str = match info.status {
                        crate::task_manager::TaskStatus::Running => "running",
                        crate::task_manager::TaskStatus::Completed => "completed",
                        crate::task_manager::TaskStatus::Failed => "failed",
                    };
                    return WaitForResult {
                        json_text: serde_json::to_string_pretty(&json!({
                            "task_id": task_id,
                            "status": status_str,
                            "progress": info.progress,
                            "progress_message": info.progress_message,
                            "elapsed_secs": info.elapsed_secs(),
                            "timeout": true,
                        }))
                        .unwrap_or_default(),
                        is_error: true,
                        task_is_project_completed: false,
                    };
                }
                None => {
                    return WaitForResult {
                        json_text: serde_json::to_string_pretty(&json!({
                            "error": format!("Task not found: {task_id}")
                        }))
                        .unwrap_or_default(),
                        is_error: true,
                        task_is_project_completed: false,
                    };
                }
            }
        }

        std::thread::sleep(Duration::from_secs(poll_interval_secs));
    }
}
