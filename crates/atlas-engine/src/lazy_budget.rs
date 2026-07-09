use std::sync::atomic::{AtomicBool, Ordering};

/// Window-level budget for cancellable on-demand (lazy) extraction.
///
/// Mechanism type: CS “lazy” = work stops when the enclosing Focus window budget
/// is exhausted. Tracks wall-clock time (ms since creation). Focus passes one
/// instance through every structural extraction in a `FocusWindow`, so individual
/// files cannot reset the enclosing query window.
#[derive(Debug)]
pub struct LazyBudget {
    budget_ms: u64,
    start: std::time::Instant,
    /// Set when budget is exhausted — consumed by CancelCheck impl.
    cancelled: AtomicBool,
}

impl LazyBudget {
    /// Create a structural budget using the caller's enclosing time window.
    ///
    /// Focus windows already carry separate foreground/background wall-clock
    /// budgets; using that duration keeps extraction cancellation aligned with
    /// the query window instead of resetting a fresh 18s budget per file.
    pub(crate) fn for_duration_ms(budget_ms: u64) -> Self {
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
        self.elapsed_ms() >= self.budget_ms as u128
    }
}

impl extraction::CancelCheck for LazyBudget {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.time_exceeded()
    }
}

#[cfg(test)]
mod tests {
    use super::LazyBudget;
    use extraction::CancelCheck;

    #[test]
    fn duration_budget_is_cancelled_when_zero() {
        let budget = LazyBudget::for_duration_ms(0);

        assert!(
            budget.is_cancelled(),
            "a zero-duration caller window must cancel extraction immediately"
        );
    }

    #[test]
    fn positive_duration_budget_starts_active() {
        let budget = LazyBudget::for_duration_ms(18_000);

        assert!(
            !budget.is_cancelled(),
            "a positive-duration caller window should not start cancelled"
        );
    }
}
