//! ProjectWriteCoordinator — serializes write access to the project DB.
//!
//! # Design
//!
//! Only one writer (focus job, lazy structural, full index, MCP sync)
//! can write at a time. Sync priority preempts background jobs via a
//! cancellation flag that background workers must poll at checkpoints.
//!
//! # Guards
//!
//! - [`WriteGuard`] — non-exclusive write access with recorded priority.
//! - [`ExclusiveGuard`] — exclusive access (e.g., `atlas index --full`),
//!   which cancels all background work on acquisition.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use super::scheduler::FocusPriority;

/// Serializes write access to the project DB.
///
/// Only one writer (focus job, lazy structural, full index, MCP sync)
/// can write at a time. Sync priority preempts background jobs.
pub struct ProjectWriteCoordinator {
    lock: Mutex<()>,
    background_cancelled: AtomicBool,
}

impl ProjectWriteCoordinator {
    pub fn new() -> Self {
        ProjectWriteCoordinator {
            lock: Mutex::new(()),
            background_cancelled: AtomicBool::new(false),
        }
    }

    /// Acquire write access. Sync priority always wins (preempts background).
    ///
    /// - `Sync`: signals cancellation and blocks until lock is available.
    /// - `UserFocus`: signals cancellation and blocks.
    /// - `Recent` / `Speculative`: blocks until lock is available (lower
    ///   priority jobs should yield when cancellation is signaled).
    pub fn acquire(&self, priority: FocusPriority) -> WriteGuard<'_> {
        match priority {
            FocusPriority::Sync => {
                self.background_cancelled.store(true, Ordering::Release);
                WriteGuard {
                    guard: self.lock.lock().expect("write coordinator lock poisoned"),
                    priority,
                }
            }
            FocusPriority::UserFocus => {
                self.background_cancelled.store(true, Ordering::Release);
                WriteGuard {
                    guard: self.lock.lock().expect("write coordinator lock poisoned"),
                    priority,
                }
            }
            _ => WriteGuard {
                guard: self.lock.lock().expect("write coordinator lock poisoned"),
                priority,
            },
        }
    }

    /// Check if background jobs should cancel.
    pub fn is_background_cancelled(&self) -> bool {
        self.background_cancelled.load(Ordering::Acquire)
    }

    /// Reset cancellation flag (called after sync job completes).
    pub fn reset_cancellation(&self) {
        self.background_cancelled.store(false, Ordering::Release);
    }

    /// Enter exclusive mode (e.g., for `atlas index --full`).
    ///
    /// No focus jobs allowed while exclusive. Signals cancellation to
    /// all background work and blocks until the lock is acquired.
    pub fn enter_exclusive(&self) -> ExclusiveGuard<'_> {
        self.background_cancelled.store(true, Ordering::Release);
        ExclusiveGuard {
            guard: self.lock.lock().expect("write coordinator lock poisoned"),
        }
    }
}

/// RAII guard for non-exclusive write access.
pub struct WriteGuard<'a> {
    guard: MutexGuard<'a, ()>,
    pub priority: FocusPriority,
}

/// RAII guard for exclusive write access.
pub struct ExclusiveGuard<'a> {
    guard: MutexGuard<'a, ()>,
}
