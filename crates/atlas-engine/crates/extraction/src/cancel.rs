//! Cancellation support for extraction.
//!
//! Provides the [`CancelCheck`] trait for cancellation-aware extraction.

/// Trait for cancellation-aware extraction.
///
/// Implementations check whether the current extraction should be
/// cancelled (budget exhausted, explicit user cancellation, etc.).
///
/// This trait lives in the `extraction` crate because extraction cannot
/// depend on `atlas-engine` budget types.
pub trait CancelCheck {
    /// Whether the current operation has been cancelled.
    fn is_cancelled(&self) -> bool;
}

impl CancelCheck for () {
    fn is_cancelled(&self) -> bool {
        false
    }
}
