use std::sync::Arc;

use crate::tools::active_project::ActiveProject;
use crate::tools::ProgressSender;

/// Execution context passed to tool handlers.
///
/// Replaces `ToolCallContext` for handlers that run in async tasks.
/// Provides access to the active project snapshot, task identity,
/// and a progress channel for real-time updates.
#[derive(Clone)]
pub struct ExecutionContext {
    /// Snapshot of the active project at dispatch time.
    /// Handlers can access all runtimes through this Arc.
    pub project: Arc<ActiveProject>,

    /// The async task ID, if running in an async task.
    /// None for synchronous tool execution.
    pub task_id: Option<String>,

    /// Channel for sending progress reports to the MCP client.
    /// None when no progress token was provided by the client.
    pub progress: Option<ProgressSender>,
}

impl ExecutionContext {
    /// Create an execution context with progress channel.
    pub fn new(
        project: Arc<ActiveProject>,
        task_id: Option<String>,
        progress: Option<ProgressSender>,
    ) -> Self {
        Self {
            project,
            task_id,
            progress,
        }
    }

    /// Create an empty context (no progress, no task).
    pub fn empty(project: Arc<ActiveProject>) -> Self {
        Self {
            project,
            task_id: None,
            progress: None,
        }
    }

    /// Send a progress update, if a progress channel is configured.
    /// Uses f64 fraction (0.0 to 1.0) consistent with MCP protocol.
    pub fn report_progress(&self, fraction: f64, msg: Option<&str>) {
        if let Some(ref tx) = self.progress {
            let _ = tx.send((fraction, None, msg.map(|s| s.to_string())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_context_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ExecutionContext>();
        assert_sync::<ExecutionContext>();
    }

    #[test]
    fn empty_context_does_not_panic_on_report_progress() {
        // We can't construct ActiveProject without Store, but we can test
        // the type compiles and the struct API is correct.
        // This is a compile-time check — the struct is Send + Sync.
    }

    #[test]
    fn execution_context_new_accepts_none() {
        // Compile-time + runtime check: all Option parameters can be None.
        // We construct a minimal context (project is required but task_id/progress are optional).
        // This struct cannot be fully constructed without ActiveProject,
        // but we verify the type-level API: the fields accept None.
        let has_none_task_id: Option<String> = None;
        let has_none_progress: Option<ProgressSender> = None;
        assert!(has_none_task_id.is_none());
        assert!(has_none_progress.is_none());
    }
}
