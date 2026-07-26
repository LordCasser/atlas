//! Atlas progress protocol — shared between CLI TUI rendering and the
//! index/sync pipeline.
//!
//! ## Architecture
//!
//! This module defines the types used to report indexing progress from a
//! worker thread to the main thread's TUI renderer.  All types are
//! dependency-free (no `ratatui`, no `crossterm`) so the engine layer can
//! depend on them without pulling in terminal libraries.
//!
//! - `ProgressPhase` — the 9 pipeline stages.
//! - `ProgressState` — thread-safe accumulator (AtomicU64 counters + Mutex).
//! - `ProgressSnapshot` — a point-in-time read-only view for the TUI.
//!
//! ## Thread safety
//!
//! The worker thread writes `ProgressState` fields (AtomicU64 for counters,
//! Mutex for phase transitions).  The main thread reads a snapshot every
//! 200 ms via `ProgressState::snapshot()`.  Lock contention is minimal:
//! the Mutex is held only during `start_phase()` / `finish_phase()`, never
//! during hot-path `increment()` calls.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Sliding window used to compute the displayed items/second rate.
///
/// Long enough to span several batch completions (DbWrite/Resolution commit
/// in 500-item chunks), short enough that a stalled counter decays to zero
/// within a few seconds instead of drifting down over the whole phase.
const RATE_WINDOW: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Phase enum
// ---------------------------------------------------------------------------

/// Pipeline phase — matches the 9-stage index flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgressPhase {
    Discovery,
    HashCheck,
    Cleanup,
    LanguageInit,
    Extraction,
    DbWrite,
    Resolution,
    EdgeBuilding,
    Finalizing,
}

impl ProgressPhase {
    /// User-facing display name (matches CodeGraph naming).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Discovery => "Scanning files",
            Self::HashCheck => "Computing hashes",
            Self::Cleanup => "Cleaning stale data",
            Self::LanguageInit => "Initializing languages",
            Self::Extraction => "Parsing code",
            Self::DbWrite => "Storing data",
            Self::Resolution => "Resolving refs",
            Self::EdgeBuilding => "Building edges",
            Self::Finalizing => "Finalizing",
        }
    }

    /// Whether this phase has a known total from the start.
    pub fn has_total(self) -> bool {
        match self {
            Self::Discovery | Self::LanguageInit | Self::Finalizing => false,
            Self::HashCheck
            | Self::Cleanup
            | Self::Extraction
            | Self::DbWrite
            | Self::Resolution
            | Self::EdgeBuilding => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase state machine
// ---------------------------------------------------------------------------

/// The state of one phase in the pipeline timeline.
#[derive(Debug, Clone)]
pub enum PhaseState {
    /// Not yet started.
    Pending,
    /// Currently executing. `started_at` is when `start_phase()` was called.
    Running {
        started_at: Instant,
        has_total: bool,
    },
    /// Finished successfully.
    Completed {
        started_at: Instant,
        finished_at: Instant,
        elapsed: Duration,
        note: Option<String>,
    },
}

impl PhaseState {
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    pub fn elapsed(&self) -> Option<Duration> {
        match self {
            Self::Running { started_at, .. } => Some(started_at.elapsed()),
            Self::Completed { elapsed, .. } => Some(*elapsed),
            Self::Pending => None,
        }
    }
}

/// A single entry in the phase timeline.
#[derive(Debug, Clone)]
pub struct PhaseEntry {
    pub phase: ProgressPhase,
    pub state: PhaseState,
}

// ---------------------------------------------------------------------------
// ProgressState — shared accumulator
// ---------------------------------------------------------------------------

/// Thread-safe progress state owned by the main thread and read by the
/// TUI render loop.  The worker thread writes through the methods below.
pub struct ProgressState {
    /// Ordered phase timeline (includes completed + current + pending).
    pub(crate) phases: Vec<PhaseEntry>,

    /// Atomic counter for the current phase — used by parallel stages
    /// (rayon) where the worker thread increments lock-free.
    pub atomic_current: Arc<AtomicU64>,

    /// Total items in the current phase.  Set during `start_phase()` for
    /// phases with a known total, or during the Phase-1→Phase-2
    /// transition for phases that discover their total later.
    pub current_total: Option<u64>,

    /// Current-phase message (e.g. current file name).
    pub message: Option<String>,

    /// Speed calculation state — updated by `flush()`.
    pub start_time: Instant,
    pub current_rate: Option<f64>, // items per second

    /// Recent `(observed_at, counter_value)` samples used to derive
    /// `current_rate` over a sliding window.  Reset on every phase change.
    pub(crate) rate_samples: VecDeque<(Instant, u64)>,

    /// Whether we've entered Phase 2 (serial-write) of a phase that
    /// started with Phase 1 (parallel-match).  When true, the TUI
    /// renders a percentage bar instead of a spinner + rate.
    pub(crate) phase2_active: bool,
}

impl ProgressState {
    /// Create a fresh state with all 9 phases in Pending.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let all_phases = [
            ProgressPhase::Discovery,
            ProgressPhase::HashCheck,
            ProgressPhase::Cleanup,
            ProgressPhase::LanguageInit,
            ProgressPhase::Extraction,
            ProgressPhase::DbWrite,
            ProgressPhase::Resolution,
            ProgressPhase::EdgeBuilding,
            ProgressPhase::Finalizing,
        ];
        Self {
            phases: all_phases
                .iter()
                .map(|&p| PhaseEntry {
                    phase: p,
                    state: PhaseState::Pending,
                })
                .collect(),
            atomic_current: Arc::new(AtomicU64::new(0)),
            current_total: None,
            message: None,
            start_time: Instant::now(),
            current_rate: None,
            rate_samples: VecDeque::new(),
            phase2_active: false,
        }
    }

    // ── Worker-thread write methods ──────────────────────────────────────

    /// Transition to a new phase.  Completes the previous running phase
    /// (if any) and starts `phase`.
    pub fn start_phase(&mut self, phase: ProgressPhase, note: Option<String>) {
        let now = Instant::now();

        // Complete previous running phase
        for entry in &mut self.phases {
            if let PhaseState::Running { started_at, .. } = entry.state {
                entry.state = PhaseState::Completed {
                    started_at,
                    finished_at: now,
                    elapsed: now.duration_since(started_at),
                    note,
                };
                break;
            }
        }

        // Start new phase
        for entry in &mut self.phases {
            if entry.phase == phase {
                entry.state = PhaseState::Running {
                    started_at: now,
                    has_total: phase.has_total(),
                };
                break;
            }
        }

        // Reset per-phase counters
        self.atomic_current.store(0, Ordering::Relaxed);
        self.current_total = if phase.has_total() {
            // total will be set later via set_total()
            None
        } else {
            None
        };
        self.message = None;
        self.current_rate = None;
        self.rate_samples.clear();
        self.phase2_active = false;
    }

    /// Set the current value directly (for serial phases).
    pub fn set_current(&mut self, current: u64) {
        self.atomic_current.store(current, Ordering::Relaxed);
    }

    /// Lock-free increment (for parallel phases — called from rayon threads).
    pub fn increment(&self) {
        self.atomic_current.fetch_add(1, Ordering::Relaxed);
    }

    /// Add `n` to current (batch update from rayon aggregation).
    pub fn add(&self, n: u64) {
        self.atomic_current.fetch_add(n, Ordering::Relaxed);
    }

    /// Set the total for the current phase (usually called at start_phase
    /// or at the Phase-1→Phase-2 transition).
    pub fn set_total(&mut self, total: u64) {
        self.current_total = Some(total);
    }

    /// Enter Phase 2 (serial-write with known total).  The TUI switches
    /// from spinner+rate to percentage bar.
    pub fn enter_phase2(&mut self, total: u64) {
        self.current_total = Some(total);
        self.phase2_active = true;
        self.atomic_current.store(0, Ordering::Relaxed);
        self.current_rate = None;
        self.rate_samples.clear();
    }

    pub fn set_message(&mut self, msg: String) {
        self.message = Some(msg);
    }

    // ── Main-thread read method ──────────────────────────────────────────

    /// Called by the main thread every 200 ms.  Reads the AtomicU64 counter,
    /// calculates rate, and returns a snapshot for the TUI renderer.
    pub fn flush_and_snapshot(&mut self) -> ProgressSnapshot {
        let now = Instant::now();
        let current = self.atomic_current.load(Ordering::Relaxed);

        // Calculate rate (items/s) over a sliding window rather than the
        // whole phase.  A whole-phase average (items ÷ phase elapsed) keeps
        // reporting throughput long after the counter has stopped moving:
        // the numerator is frozen while the denominator grows, producing a
        // slow hyperbolic decay that looks like "inertia" instead of a stall.
        // The window is wide enough (RATE_WINDOW) to smooth over batch-updating
        // phases like DbWrite/Resolution/EdgeBuilding, which jump by 500+ items
        // at irregular intervals, while still decaying to 0 when work stalls.
        self.rate_samples.push_back((now, current));
        while let Some(&(t, _)) = self.rate_samples.front() {
            if now.duration_since(t) > RATE_WINDOW {
                self.rate_samples.pop_front();
            } else {
                break;
            }
        }
        self.current_rate = match (self.rate_samples.front(), self.rate_samples.back()) {
            (Some(&(t0, c0)), Some(&(t1, c1))) => {
                let span = t1.duration_since(t0).as_secs_f64();
                // Require a minimum span so the first few 200ms ticks don't
                // produce wildly overstated rates.
                if span >= 0.5 {
                    Some(c1.saturating_sub(c0) as f64 / span)
                } else {
                    self.current_rate
                }
            }
            _ => self.current_rate,
        };

        // Find the currently running phase
        let current_phase = self
            .phases
            .iter()
            .find(|e| e.state.is_running())
            .map(|e| e.phase);

        // Collect completed phases
        let completed: Vec<CompletedPhase> = self
            .phases
            .iter()
            .filter_map(|e| {
                if let PhaseState::Completed { elapsed, note, .. } = &e.state {
                    Some(CompletedPhase {
                        phase: e.phase,
                        elapsed: *elapsed,
                        note: note.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

        ProgressSnapshot {
            current_phase,
            current,
            total: self.current_total,
            phase2_active: self.phase2_active,
            message: self.message.clone(),
            rate: self.current_rate,
            elapsed: now.duration_since(self.start_time),
            completed,
            phases: self.phases.clone(),
        }
    }

    /// Read-only snapshot without side effects (for error/interrupt paths).
    pub fn read_snapshot(&self) -> ProgressSnapshot {
        let now = Instant::now();
        let current = self.atomic_current.load(Ordering::Relaxed);

        let current_phase = self
            .phases
            .iter()
            .find(|e| e.state.is_running())
            .map(|e| e.phase);

        let completed: Vec<CompletedPhase> = self
            .phases
            .iter()
            .filter_map(|e| {
                if let PhaseState::Completed { elapsed, note, .. } = &e.state {
                    Some(CompletedPhase {
                        phase: e.phase,
                        elapsed: *elapsed,
                        note: note.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

        ProgressSnapshot {
            current_phase,
            current,
            total: self.current_total,
            phase2_active: self.phase2_active,
            message: self.message.clone(),
            rate: self.current_rate,
            elapsed: now.duration_since(self.start_time),
            completed,
            phases: self.phases.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot (read-only, for TUI)
// ---------------------------------------------------------------------------

/// Point-in-time read-only view for the TUI renderer.
#[derive(Debug, Clone)]
pub struct ProgressSnapshot {
    /// Which phase is currently running (None if nothing started).
    pub current_phase: Option<ProgressPhase>,
    /// Current item count.
    pub current: u64,
    /// Total item count (None during Phase 1 of mixed-mode phases).
    pub total: Option<u64>,
    /// Whether we're in Phase 2 (serial-write with percentage bar).
    pub phase2_active: bool,
    /// Current-phase message.
    pub message: Option<String>,
    /// Items/second (smoothed).
    pub rate: Option<f64>,
    /// Total elapsed since pipeline start.
    pub elapsed: Duration,
    /// Completed phases and their timing.
    pub completed: Vec<CompletedPhase>,
    /// Full phase list (for rendering the timeline).
    pub phases: Vec<PhaseEntry>,
}

impl ProgressSnapshot {
    /// Percentage complete for the current phase (0..100).  None if total unknown.
    pub fn percent(&self) -> Option<f64> {
        self.total.map(|t| {
            if t == 0 {
                100.0
            } else {
                (self.current as f64 / t as f64) * 100.0
            }
        })
    }
}

/// A completed phase entry in the snapshot.
#[derive(Debug, Clone)]
pub struct CompletedPhase {
    pub phase: ProgressPhase,
    pub elapsed: Duration,
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Backdate every timestamp in the rate window so tests do not sleep.
    fn backdate(state: &mut ProgressState, by: Duration) {
        for sample in &mut state.rate_samples {
            sample.0 -= by;
        }
    }

    #[test]
    fn rate_reflects_recent_throughput_not_phase_average() {
        let mut state = ProgressState::new();
        state.start_phase(ProgressPhase::EdgeBuilding, None);

        state.set_current(0);
        state.flush_and_snapshot();
        backdate(&mut state, Duration::from_secs(1));

        state.set_current(1000);
        let snap = state.flush_and_snapshot();
        let rate = snap.rate.expect("rate should be available");
        assert!(
            (rate - 1000.0).abs() < 50.0,
            "expected ~1000 items/s, got {rate}"
        );
    }

    #[test]
    fn stalled_counter_decays_rate_to_zero() {
        let mut state = ProgressState::new();
        state.start_phase(ProgressPhase::EdgeBuilding, None);

        // Ramp up to a non-zero rate.
        state.set_current(0);
        state.flush_and_snapshot();
        backdate(&mut state, Duration::from_secs(1));
        state.set_current(1000);
        assert!(state.flush_and_snapshot().rate.unwrap() > 0.0);

        // Counter stops advancing: samples still arrive, value never changes.
        // Once every in-window sample carries the same value the rate is 0,
        // rather than the old phase-average behaviour that decayed slowly
        // and made a stalled phase look like it was still making progress.
        for _ in 0..3 {
            backdate(&mut state, RATE_WINDOW);
            state.flush_and_snapshot();
        }
        backdate(&mut state, Duration::from_secs(1));
        let snap = state.flush_and_snapshot();
        assert_eq!(
            snap.rate,
            Some(0.0),
            "stalled phase should report 0 items/s"
        );
    }

    #[test]
    fn rate_resets_between_phases() {
        let mut state = ProgressState::new();
        state.start_phase(ProgressPhase::Resolution, None);
        state.set_current(0);
        state.flush_and_snapshot();
        backdate(&mut state, Duration::from_secs(1));
        state.set_current(500);
        state.flush_and_snapshot();

        state.start_phase(ProgressPhase::EdgeBuilding, None);
        assert!(state.rate_samples.is_empty());
        assert_eq!(state.current_rate, None);
    }
}
