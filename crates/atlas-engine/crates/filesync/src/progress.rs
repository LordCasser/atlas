//! Pluggable progress reporting for index and sync pipelines.
//!
//! This module defines [`ProgressSink`] — a trait that decouples pipeline
//! progress from any specific UI or protocol.  Concrete implementations
//! translate events into callbacks, MCP notifications, TUI progress bars,
//! or structured logging without the pipeline knowing which is in use.
//!
//! # Design
//! - **Event-driven**: each phase boundary, item completion, warning, or
//!   cancellation produces a single [`ProgressEvent`].
//! - **Sink-oriented**: the pipeline calls `sink.emit(event)`; the sink
//!   owns all rendering / forwarding / aggregation logic.
//! - **No control flow**: `ProgressSink` is a pure data sink.  Cancellation
//!   is managed through a separate `FnMut() -> bool` interrupt closure,
//!   keeping progress reporting and control flow orthogonal.

use crate::index_pipeline::{IndexProgress, IndexProgressCallback};
use std::fmt;
use std::sync::{Arc, Mutex};
use types::progress::ProgressState;

// ── ProgressEvent ──────────────────────────────────────────────────────────

/// A single progress event emitted by an index or sync pipeline.
///
/// Pipelines emit these at well-defined lifecycle points:
/// - [`PhaseStarted`] when entering a named phase (discovery, extraction, …).
/// - [`ItemProgress`] after processing each item (or every N items for
///   high-throughput phases).
/// - [`PhaseFinished`] when exiting a phase (includes aggregate stats).
/// - [`Warning`] for non-fatal conditions the caller may surface.
/// - [`Cancelled`] if the pipeline was interrupted before completion.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// Pipeline entered a new phase.
    PhaseStarted {
        phase: PhaseName,
        /// Total work items in this phase (`0` if unknown).
        total: u64,
    },
    /// Progress within a phase (item-level granularity).
    ItemProgress {
        phase: PhaseName,
        /// Items completed so far.
        completed: u64,
    },
    /// Pipeline completed a phase.
    PhaseFinished {
        phase: PhaseName,
        /// Items that succeeded in this phase.
        succeeded: u64,
        /// Items that failed in this phase.
        failed: u64,
        /// Optional human-readable detail (e.g. "42 files, 3 failed").
        detail: Option<String>,
    },
    /// Non-fatal condition (e.g. parse warning, deprecated feature).
    Warning { phase: PhaseName, message: String },
    /// Pipeline was cancelled before completion.
    Cancelled {
        /// Last phase that produced partial results.
        last_phase: PhaseName,
    },
}

// ── PhaseName ──────────────────────────────────────────────────────────────

/// Well-known pipeline phases.
///
/// String conversion is cheap; callers can match or format these as needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhaseName {
    Discovery,
    HashCheck,
    Cleanup,
    LanguageInit,
    Extraction,
    DbWrite,
    Resolution,
    EdgeBuild,
    AnnotationMaterialize,
    SummaryBuild,
    Finalize,
    /// Catch-all for phases not in the standard set.
    Custom(&'static str),
}

impl fmt::Display for PhaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PhaseName::Discovery => write!(f, "Discovery"),
            PhaseName::HashCheck => write!(f, "HashCheck"),
            PhaseName::Cleanup => write!(f, "Cleanup"),
            PhaseName::LanguageInit => write!(f, "LanguageInit"),
            PhaseName::Extraction => write!(f, "Extraction"),
            PhaseName::DbWrite => write!(f, "DbWrite"),
            PhaseName::Resolution => write!(f, "Resolution"),
            PhaseName::EdgeBuild => write!(f, "EdgeBuild"),
            PhaseName::AnnotationMaterialize => write!(f, "AnnotationMaterialize"),
            PhaseName::SummaryBuild => write!(f, "SummaryBuild"),
            PhaseName::Finalize => write!(f, "Finalize"),
            PhaseName::Custom(s) => write!(f, "{s}"),
        }
    }
}

// ── ProgressSink ───────────────────────────────────────────────────────────

/// A sink that receives [`ProgressEvent`]s from a pipeline.
///
/// Implementations translate events into protocol messages, UI updates,
/// log records, or no-ops.  The pipeline never knows which sink it is
/// feeding; it simply calls `emit()` at well-defined boundaries.
///
/// The trait is object-safe by design — callers pass `Box<dyn ProgressSink>`.
pub trait ProgressSink: Send + Sync {
    /// Receive a progress event.
    fn emit(&self, event: ProgressEvent);

    /// Returns an optional reference to the internal [`ProgressState`]
    /// that this sink manages. Default returns `None`.
    ///
    /// This allows phase functions that use [`ProgressState`] natively
    /// (e.g. the parallel resolver) to update progress directly without
    /// going through [`ProgressEvent::ItemProgress`] translation.
    fn progress_state(&self) -> Option<&Arc<Mutex<ProgressState>>> {
        None
    }
}

// ── Built-in implementations ───────────────────────────────────────────────

/// A sink that forwards every event to a legacy
/// [`IndexProgressCallback`].
///
/// This bridges the old callback-based API so existing callers
/// (MCP, CLI) can migrate without changing their callback logic.
pub struct CallbackSink {
    callback: IndexProgressCallback,
}

impl CallbackSink {
    /// Wrap an existing callback in a `CallbackSink`.
    pub fn new(callback: IndexProgressCallback) -> Self {
        Self { callback }
    }
}

impl ProgressSink for CallbackSink {
    fn emit(&self, event: ProgressEvent) {
        // Forward PhaseFinished events to the legacy callback as a fraction
        // update.  Rich event types (PhaseStarted, Warning, Cancelled) are
        // silently dropped — legacy callers only understand fractions.
        if let ProgressEvent::PhaseFinished {
            succeeded,
            failed,
            detail,
            ..
        } = event
        {
            let total = succeeded + failed;
            (self.callback)(IndexProgress {
                fraction: 1.0, // legacy callers interpret 1.0 = "phase done"
                total: Some(total as f64),
                message: detail,
            });
        }
    }
}

/// A sink that silently discards all events (no-op).
///
/// Use when progress reporting is not needed (e.g. batch scripts,
/// tests, headless automation).
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn emit(&self, _event: ProgressEvent) {}
}

// ── Multiplex sink ─────────────────────────────────────────────────────────

/// A sink that broadcasts every event to multiple child sinks.
///
/// Useful when you want to log progress AND send protocol notifications
/// simultaneously — compose two sinks into one.
pub struct MultiplexSink {
    children: Vec<Box<dyn ProgressSink>>,
}

impl MultiplexSink {
    pub fn new(children: Vec<Box<dyn ProgressSink>>) -> Self {
        Self { children }
    }
}

impl ProgressSink for MultiplexSink {
    fn emit(&self, event: ProgressEvent) {
        for child in &self.children {
            child.emit(event.clone());
        }
    }

    fn progress_state(&self) -> Option<&Arc<Mutex<ProgressState>>> {
        for child in &self.children {
            if let Some(ps) = child.progress_state() {
                return Some(ps);
            }
        }
        None
    }
}

// ── Convenience helpers ────────────────────────────────────────────────────

/// Create a boxed no-op sink.
pub fn noop() -> Box<dyn ProgressSink> {
    Box::new(NoopSink)
}

/// Create a boxed callback sink from a legacy callback.
pub fn from_callback(cb: IndexProgressCallback) -> Box<dyn ProgressSink> {
    Box::new(CallbackSink::new(cb))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::index_pipeline::IndexProgress;

    #[test]
    fn noop_sink_discards_all() {
        let sink = noop();
        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::Discovery,
            total: 100,
        });
        sink.emit(ProgressEvent::Cancelled {
            last_phase: PhaseName::Extraction,
        });
        // No assertions needed — the sink must not panic.
    }

    #[test]
    fn callback_sink_forwards_phase_finished() {
        let called = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&called);
        let cb: IndexProgressCallback = Arc::new(move |p: IndexProgress| {
            assert!(p.fraction > 0.0);
            assert!(p.total.is_some());
            c.store(true, Ordering::SeqCst);
        });
        let sink = from_callback(cb);
        sink.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::Extraction,
            succeeded: 42,
            failed: 3,
            detail: Some("done".into()),
        });
        assert!(called.load(Ordering::SeqCst));
    }

    #[test]
    fn callback_sink_ignores_non_finished_events() {
        let called = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&called);
        let cb: IndexProgressCallback = Arc::new(move |_| {
            c.store(true, Ordering::SeqCst);
        });
        let sink = from_callback(cb);
        // PhaseStarted, ItemProgress, Warning, Cancelled should not trigger callback.
        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::Extraction,
            total: 50,
        });
        sink.emit(ProgressEvent::Warning {
            phase: PhaseName::Extraction,
            message: "test".into(),
        });
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn multiplex_sends_to_all_children() {
        // Collect events into shared vectors to verify both children received them.
        let events1 = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events2 = Arc::new(std::sync::Mutex::new(Vec::new()));

        let e1_clone = Arc::clone(&events1);
        let e2_clone = Arc::clone(&events2);

        struct CollectingSink {
            events: Arc<std::sync::Mutex<Vec<ProgressEvent>>>,
        }
        impl ProgressSink for CollectingSink {
            fn emit(&self, event: ProgressEvent) {
                self.events.lock().unwrap().push(event);
            }
        }

        let multi = MultiplexSink::new(vec![
            Box::new(CollectingSink { events: e1_clone }),
            Box::new(CollectingSink { events: e2_clone }),
        ]);

        multi.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::DbWrite,
            succeeded: 42,
            failed: 3,
            detail: Some("done".into()),
        });

        assert_eq!(events1.lock().unwrap().len(), 1);
        assert_eq!(events2.lock().unwrap().len(), 1);
    }

    #[test]
    fn phase_name_display() {
        assert_eq!(PhaseName::Discovery.to_string(), "Discovery");
        assert_eq!(PhaseName::HashCheck.to_string(), "HashCheck");
        assert_eq!(PhaseName::SummaryBuild.to_string(), "SummaryBuild");
        assert_eq!(
            PhaseName::Custom("IncrementalDetect").to_string(),
            "IncrementalDetect"
        );
    }

    #[test]
    fn phase_name_equality() {
        assert_eq!(PhaseName::Extraction, PhaseName::Extraction);
        assert_ne!(PhaseName::Extraction, PhaseName::DbWrite);
    }
}
