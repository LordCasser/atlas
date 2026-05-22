//! Phase timing and per-language statistics for the indexing pipeline.
//!
//! These types track wall-clock time for each pipeline phase and aggregate
//! per-language file counts, timing, and error counts. They are consumed by
//! CLI output and persisted in `IndexReport`.

use crate::Language;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// PhaseTiming — a named timing span
// ---------------------------------------------------------------------------

/// A single timing measurement for a named pipeline phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseTiming {
    /// Phase name (e.g. "Discovery", "Parse/extract", "DB write").
    pub phase: String,
    /// Elapsed wall-clock time in milliseconds.
    pub duration_ms: u64,
    /// Optional item count for throughput display (e.g. file count, ref count).
    pub items: Option<u64>,
    /// Optional extra notes (e.g. "0 reused, 24 dirty").
    pub note: Option<String>,
}

impl PhaseTiming {
    pub fn new(phase: &str, duration: Duration, items: Option<u64>, note: Option<String>) -> Self {
        Self {
            phase: phase.to_string(),
            duration_ms: duration.as_millis() as u64,
            items,
            note,
        }
    }
}

// ---------------------------------------------------------------------------
// PhaseTimer — live timer for a single phase (not serializable, used during
// pipeline execution)
// ---------------------------------------------------------------------------

/// RAII-style timer that records a `PhaseTiming` on finish.
pub struct PhaseTimer {
    phase: String,
    start: Instant,
    items: Option<u64>,
    note: Option<String>,
    result: Option<PhaseTiming>,
}

impl PhaseTimer {
    /// Start timing a phase.
    pub fn start(phase: &str) -> Self {
        Self {
            phase: phase.to_string(),
            start: Instant::now(),
            items: None,
            note: None,
            result: None,
        }
    }

    /// Set the item count for this phase.
    pub fn items(mut self, count: u64) -> Self {
        self.items = Some(count);
        self
    }

    /// Set a note for this phase.
    pub fn note(mut self, note: String) -> Self {
        self.note = Some(note);
        self
    }

    /// Finish timing and consume the timer, returning a `PhaseTiming`.
    pub fn finish(mut self) -> PhaseTiming {
        let duration = self.start.elapsed();
        let timing = PhaseTiming::new(&self.phase, duration, self.items, self.note.take());
        self.result = Some(timing.clone());
        timing
    }
}

// ---------------------------------------------------------------------------
// PhaseTimings — collection of all phase timings for a pipeline run
// ---------------------------------------------------------------------------

/// Ordered collection of phase timings for a single index/sync run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseTimings {
    pub phases: Vec<PhaseTiming>,
    /// Total wall-clock time for the entire pipeline (ms).
    pub total_ms: u64,
}

impl PhaseTimings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a completed phase timing.
    pub fn push(&mut self, timing: PhaseTiming) {
        self.phases.push(timing);
    }

    /// Set the total duration (computed from outer timer).
    pub fn set_total(&mut self, duration: Duration) {
        self.total_ms = duration.as_millis() as u64;
    }

    /// Whether any phases have been recorded.
    pub fn is_empty(&self) -> bool {
        self.phases.is_empty()
    }
}

// ---------------------------------------------------------------------------
// PerLanguageStats — per-language aggregation
// ---------------------------------------------------------------------------

/// Statistics for files of a single language.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LanguageEntry {
    /// Number of files processed for this language.
    pub file_count: usize,
    /// Total extraction time for this language (ms).
    pub extract_ms: u64,
    /// Number of files that failed extraction.
    pub failures: usize,
    /// Failure counts by category for this language.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failures_by_category: BTreeMap<String, usize>,
}

/// Per-language statistics collected during indexing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerLanguageStats {
    /// Keyed by `Language::as_str()`.
    pub languages: BTreeMap<String, LanguageEntry>,
}

impl PerLanguageStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a file processed for a language.
    pub fn record_file(
        &mut self,
        lang: Language,
        extract_ms: u64,
        failed: bool,
        failure_category: Option<&str>,
    ) {
        let key = lang.as_str().to_string();
        let entry = self.languages.entry(key).or_default();
        entry.file_count += 1;
        entry.extract_ms += extract_ms;
        if failed {
            entry.failures += 1;
            if let Some(cat) = failure_category {
                *entry
                    .failures_by_category
                    .entry(cat.to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_timer_records_duration() {
        let timing = PhaseTimer::start("Discovery")
            .items(24)
            .note("test".to_string())
            .finish();
        assert_eq!(timing.phase, "Discovery");
        assert_eq!(timing.items, Some(24));
        assert_eq!(timing.note, Some("test".to_string()));
        assert!(timing.duration_ms < 1000); // should complete quickly
    }

    #[test]
    fn test_phase_timings_collection() {
        let mut timings = PhaseTimings::new();
        timings.push(PhaseTimer::start("Phase A").items(10).finish());
        timings.push(PhaseTimer::start("Phase B").items(5).finish());
        timings.set_total(Duration::from_millis(300));
        assert_eq!(timings.phases.len(), 2);
        assert_eq!(timings.total_ms, 300);
    }

    #[test]
    fn test_per_language_stats() {
        let mut stats = PerLanguageStats::new();
        stats.record_file(Language::TypeScript, 100, false, None);
        stats.record_file(Language::TypeScript, 200, true, Some("query_error"));
        stats.record_file(Language::Python, 50, false, None);

        let ts_entry = stats.languages.get("typescript").unwrap();
        assert_eq!(ts_entry.file_count, 2);
        assert_eq!(ts_entry.extract_ms, 300);
        assert_eq!(ts_entry.failures, 1);
        assert_eq!(ts_entry.failures_by_category.get("query_error"), Some(&1));

        let py_entry = stats.languages.get("python").unwrap();
        assert_eq!(py_entry.file_count, 1);
        assert_eq!(py_entry.extract_ms, 50);
        assert_eq!(py_entry.failures, 0);

        assert!(!stats.is_empty());
    }
}
