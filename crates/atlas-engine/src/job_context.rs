//! Unified context for long-running jobs across all entry points (CLI, MCP, TUI).
//!
//! Each job receives a `JobContext` that bundles:
//! - A progress sink for reporting progress events,
//! - A cancellation token shared between the caller and the worker,
//! - An optional task ID for job tracking.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ProgressSink;

/// Context shared between a long-running job and its caller.
///
/// Used by CLI progress, MCP background tasks, and TUI jobs.
pub struct JobContext {
    /// Sink that receives progress events from the job.
    pub progress: Box<dyn ProgressSink>,
    /// Shared cancellation token — the caller sets this to `true` to request
    /// cancellation; the worker polls it periodically.
    pub cancel: Arc<AtomicBool>,
    /// Optional task identifier for job tracking/dedup.
    pub task_id: Option<String>,
}

impl JobContext {
    /// Create a new context with a fresh cancellation token.
    pub fn new(progress: Box<dyn ProgressSink>) -> Self {
        Self {
            progress,
            cancel: Arc::new(AtomicBool::new(false)),
            task_id: None,
        }
    }

    /// Create a context that shares an existing cancellation token.
    pub fn with_cancel(progress: Box<dyn ProgressSink>, cancel: Arc<AtomicBool>) -> Self {
        Self {
            progress,
            cancel,
            task_id: None,
        }
    }

    /// Attach a task ID to this context for tracking.
    pub fn with_task_id(mut self, task_id: String) -> Self {
        self.task_id = Some(task_id);
        self
    }

    /// Check whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}
