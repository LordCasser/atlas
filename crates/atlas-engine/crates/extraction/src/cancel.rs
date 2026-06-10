//! Cancellation support for extraction.
//!
//! Provides the [`CancelCheck`] trait for cancellation-aware extraction
//! and a [`NeverCancel`] sentinel for backward compatibility.

/// Trait for cancellation-aware extraction.
///
/// Implementations check whether the current extraction should be
/// cancelled (budget exhausted, explicit user cancellation, etc.).
///
/// This trait lives in the `extraction` crate because it is consumed
/// by `extract_file_with_mode_cancellable` — extraction cannot depend
/// on `atlas-engine`.
pub trait CancelCheck {
    /// Whether the current operation has been cancelled.
    fn is_cancelled(&self) -> bool;
}

/// A CancelCheck that never cancels — used by the original
/// `extract_file_with_mode` wrapper for backward compatibility.
pub(crate) struct NeverCancel;

impl CancelCheck for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}
