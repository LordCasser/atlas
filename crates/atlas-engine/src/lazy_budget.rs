use std::sync::atomic::{AtomicBool, Ordering};

/// Request-level budget for lazy extraction within a single MCP tool call.
///
/// Tracks two independent dimensions:
/// - Wall-clock time (ms since creation)
/// - File count consumed
///
/// Shared across all `LazyCoordinator` calls within one tool invocation.
#[derive(Debug)]
pub struct LazyBudget {
    budget_ms: u64,
    max_files: usize,
    start: std::time::Instant,
    files_consumed: usize,
    /// Set when budget is exhausted — consumed by CancelCheck impl.
    cancelled: AtomicBool,
}

/// Total wall-clock budget for lazy structural extraction per MCP request.
pub(crate) const LAZY_STRUCTURAL_BUDGET_MS: u64 = 18_000;

/// Maximum number of files that can be lazily extracted in a single MCP request.
pub(crate) const LAZY_STRUCTURAL_MAX_FILES: usize = 30;

impl LazyBudget {
    /// Create a new structural budget with the standard 18s / 30-file limits.
    pub fn structural() -> Self {
        Self {
            budget_ms: LAZY_STRUCTURAL_BUDGET_MS,
            max_files: LAZY_STRUCTURAL_MAX_FILES,
            start: std::time::Instant::now(),
            files_consumed: 0,
            cancelled: AtomicBool::new(false),
        }
    }

    /// Background preparse budget: longer time window but still capped to
    /// avoid unbounded background work competing with foreground requests.
    pub fn background_preparse() -> Self {
        Self {
            budget_ms: 60_000,
            max_files: 100,
            start: std::time::Instant::now(),
            files_consumed: 0,
            cancelled: AtomicBool::new(false),
        }
    }

    /// Custom budget (for testing only — not part of the stable API surface).
    #[allow(dead_code)]
    pub(crate) fn new(budget_ms: u64, max_files: usize) -> Self {
        Self {
            budget_ms,
            max_files,
            start: std::time::Instant::now(),
            files_consumed: 0,
            cancelled: AtomicBool::new(false),
        }
    }

    /// Whether extraction can continue (both time and file quotas remain).
    pub fn can_continue(&self) -> bool {
        if self.time_exceeded() {
            self.cancel();
            return false;
        }
        !self.files_exhausted() && !self.cancelled.load(Ordering::Acquire)
    }

    /// Signal cancellation (called from budget check or externally).
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Record that one file has been extracted.
    pub fn consume_file(&mut self) {
        self.files_consumed += 1;
    }

    /// Milliseconds elapsed since creation.
    pub fn elapsed_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }

    /// Files consumed so far.
    pub fn files_consumed(&self) -> usize {
        self.files_consumed
    }

    /// Whether the time budget has been exceeded.
    pub fn time_exceeded(&self) -> bool {
        self.elapsed_ms() > self.budget_ms as u128
    }

    /// Whether the file quota has been exhausted.
    pub fn files_exhausted(&self) -> bool {
        self.files_consumed >= self.max_files
    }
}

impl extraction::CancelCheck for LazyBudget {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.time_exceeded()
    }
}
