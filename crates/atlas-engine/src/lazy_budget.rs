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
        }
    }

    /// Create a new dataflow budget with the standard limits.
    pub fn dataflow() -> Self {
        // Dataflow uses unit count rather than file count, but we re-use
        // the same struct. The caller interprets max_files as max_units.
        Self {
            budget_ms: 20_000,
            max_files: 32,
            start: std::time::Instant::now(),
            files_consumed: 0,
        }
    }

    /// Custom budget (for testing or future tuning).
    pub fn new(budget_ms: u64, max_files: usize) -> Self {
        Self {
            budget_ms,
            max_files,
            start: std::time::Instant::now(),
            files_consumed: 0,
        }
    }

    /// Whether extraction can continue (both time and file quotas remain).
    pub fn can_continue(&self) -> bool {
        !self.time_exceeded() && !self.files_exhausted()
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
