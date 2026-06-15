use std::sync::atomic::{AtomicBool, Ordering};

/// Request-level budget for lazy extraction within a single MCP tool call.
///
/// Tracks wall-clock time (ms since creation). Shared across all
/// `LazyCoordinator` calls within one tool invocation.
#[derive(Debug)]
pub struct LazyBudget {
    budget_ms: u64,
    start: std::time::Instant,
    /// Set when budget is exhausted — consumed by CancelCheck impl.
    cancelled: AtomicBool,
}

/// Total wall-clock budget for lazy structural extraction per MCP request.
pub(crate) const LAZY_STRUCTURAL_BUDGET_MS: u64 = 18_000;

impl LazyBudget {
    /// Create a new structural budget with the standard 18s limit.
    pub fn structural() -> Self {
        Self {
            budget_ms: LAZY_STRUCTURAL_BUDGET_MS,
            start: std::time::Instant::now(),
            cancelled: AtomicBool::new(false),
        }
    }

    /// Custom budget (for testing only — not part of the stable API surface).
    #[cfg(test)]
    pub(crate) fn new(budget_ms: u64) -> Self {
        Self {
            budget_ms,
            start: std::time::Instant::now(),
            cancelled: AtomicBool::new(false),
        }
    }

    /// Milliseconds elapsed since creation.
    pub fn elapsed_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }

    /// Whether the time budget has been exceeded.
    pub fn time_exceeded(&self) -> bool {
        self.elapsed_ms() > self.budget_ms as u128
    }
}

impl extraction::CancelCheck for LazyBudget {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.time_exceeded()
    }
}
