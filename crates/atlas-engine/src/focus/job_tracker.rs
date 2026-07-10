use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use types::FileId;

/// Tracks completion and graph-refresh publication for background focus jobs.
///
/// Shared between [`FocusRuntime`] (which submits job IDs) and
/// [`FocusScheduler`] (which reports completions). Enables the MCP
/// layer to detect when all pending background work for a query
/// has finished (terminal state) versus when polling is still needed.
#[derive(Debug)]
pub struct JobTracker {
    /// Terminal jobs. `None` means success; `Some` retains the failure reason.
    terminal: Mutex<HashMap<String, Option<String>>>,
    /// Files materialized by each background closure.
    built_files: Mutex<HashMap<String, Vec<FileId>>>,
    /// Materialized files not yet handed to the graph refresh consumer.
    ///
    /// This is intentionally separate from `built_files`: query snapshots need
    /// stable per-job history, while graph refresh needs one-shot delivery.
    pending_refresh_files: Mutex<HashSet<FileId>>,
    /// Recorded build durations (in milliseconds) for completed jobs,
    /// used to compute ETA for pending jobs.
    build_times: Mutex<Vec<u64>>,
}

impl JobTracker {
    /// Create a new, empty tracker.
    pub fn new() -> Self {
        Self {
            terminal: Mutex::new(HashMap::new()),
            built_files: Mutex::new(HashMap::new()),
            pending_refresh_files: Mutex::new(HashSet::new()),
            build_times: Mutex::new(Vec::new()),
        }
    }

    /// Record that a job has completed its closure build.
    ///
    /// Called by [`FocusScheduler`] after a background job finishes.
    /// Idempotent — duplicate calls for the same ID are harmless.
    pub fn mark_done(&self, job_id: &str) {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id.to_string(), None);
    }

    /// Record a terminal failure so polling converges without losing cause.
    pub fn mark_failed(&self, job_id: &str, reason: impl Into<String>) {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id.to_string(), Some(reason.into()));
    }

    pub fn record_built_files(&self, job_id: &str, files: impl IntoIterator<Item = FileId>) {
        let mut unique = HashSet::new();
        let files: Vec<FileId> = files
            .into_iter()
            .filter(|file_id| unique.insert(*file_id))
            .collect();
        self.built_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id.to_string(), files.clone());
        self.pending_refresh_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(files);
    }

    pub fn built_files_for(&self, job_ids: &[String]) -> Vec<FileId> {
        let files = self.built_files.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = HashSet::new();
        job_ids
            .iter()
            .filter_map(|job_id| files.get(job_id))
            .flatten()
            .copied()
            .filter(|file_id| seen.insert(*file_id))
            .collect()
    }

    /// Take files built by background jobs since the previous drain.
    ///
    /// Per-job history remains available through [`Self::built_files_for`].
    /// This one-shot view lets a fresh request observe background writes without
    /// carrying closure IDs across requests.
    pub(crate) fn take_refresh_files(&self) -> Vec<FileId> {
        self.pending_refresh_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect()
    }

    pub fn failures_for(&self, job_ids: &[String]) -> Vec<(String, String)> {
        let terminal = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        job_ids
            .iter()
            .filter_map(|job_id| {
                terminal
                    .get(job_id)
                    .and_then(|reason| reason.as_ref())
                    .map(|reason| (job_id.clone(), reason.clone()))
            })
            .collect()
    }

    /// Check whether every job in `job_ids` has completed.
    ///
    /// When `job_ids` is empty, returns `true` (vacuously satisfied).
    pub fn are_all_done(&self, job_ids: &[String]) -> bool {
        if job_ids.is_empty() {
            return true;
        }
        let terminal = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        job_ids.iter().all(|id| terminal.contains_key(id))
    }

    /// Count how many jobs in `job_ids` are NOT yet completed.
    pub fn pending_count(&self, job_ids: &[String]) -> usize {
        if job_ids.is_empty() {
            return 0;
        }
        let terminal = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        job_ids
            .iter()
            .filter(|id| !terminal.contains_key(*id))
            .count()
    }

    /// Record the wall-clock duration of a completed job build.
    /// Used by the scheduler to feed data into ETA calculation.
    pub fn record_elapsed(&self, elapsed_ms: u64) {
        self.build_times
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(elapsed_ms);
    }

    /// Estimated time (ms) until all pending jobs complete.
    /// Formula: avg_completed_duration × pending_count.
    /// Returns baseline 5000ms when no completed samples yet.
    /// Capped at 60000ms (1 minute).
    pub fn eta_ms(&self, pending_ids: &[String]) -> u64 {
        self.pending_count_and_eta_ms(pending_ids).1
    }

    /// Snapshot the pending count and ETA against the same completion state.
    pub fn pending_count_and_eta_ms(&self, job_ids: &[String]) -> (usize, u64) {
        let terminal = self.terminal.lock().unwrap_or_else(|e| e.into_inner());
        let pending = job_ids
            .iter()
            .filter(|id| !terminal.contains_key(*id))
            .count();
        if pending == 0 {
            return (0, 0);
        }
        let times = self.build_times.lock().unwrap_or_else(|e| e.into_inner());
        if times.is_empty() {
            return (pending, (5000 * pending as u64).min(60000));
        }
        let avg: u64 = times.iter().sum::<u64>() / times.len() as u64;
        (pending, (avg * pending as u64).min(60000))
    }
}

impl Default for JobTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_has_nothing_done() {
        let tracker = JobTracker::new();
        let ids = vec!["cl_1".to_string(), "cl_2".to_string()];
        assert!(!tracker.are_all_done(&ids));
        assert_eq!(tracker.pending_count(&ids), 2);
    }

    #[test]
    fn mark_done_transitions_to_complete() {
        let tracker = JobTracker::new();
        tracker.mark_done("cl_1");
        tracker.mark_done("cl_2");

        let ids = vec!["cl_1".to_string(), "cl_2".to_string()];
        assert!(tracker.are_all_done(&ids));
        assert_eq!(tracker.pending_count(&ids), 0);
    }

    #[test]
    fn partial_completion_is_not_all_done() {
        let tracker = JobTracker::new();
        tracker.mark_done("cl_1");

        let ids = vec!["cl_1".to_string(), "cl_2".to_string()];
        assert!(!tracker.are_all_done(&ids));
        assert_eq!(tracker.pending_count(&ids), 1);
    }

    #[test]
    fn empty_job_ids_is_always_done() {
        let tracker = JobTracker::new();
        assert!(tracker.are_all_done(&[]));
        assert_eq!(tracker.pending_count(&[]), 0);
    }

    #[test]
    fn mark_done_is_idempotent() {
        let tracker = JobTracker::new();
        tracker.mark_done("cl_1");
        tracker.mark_done("cl_1");
        tracker.mark_done("cl_1");

        let ids = vec!["cl_1".to_string()];
        assert!(tracker.are_all_done(&ids));
    }

    #[test]
    fn unknown_id_not_in_tracker() {
        let tracker = JobTracker::new();
        tracker.mark_done("cl_1");

        assert!(!tracker.are_all_done(&["cl_1".to_string(), "cl_unknown".to_string()]));
        assert_eq!(
            tracker.pending_count(&["cl_1".to_string(), "cl_unknown".to_string()]),
            1
        );
    }

    #[test]
    fn test_eta_returns_zero_when_no_pending() {
        let tracker = JobTracker::new();
        let ids = vec!["cl_1".to_string()];
        tracker.mark_done("cl_1");
        assert_eq!(tracker.eta_ms(&ids), 0);
    }

    #[test]
    fn test_eta_uses_baseline_when_no_history() {
        let tracker = JobTracker::new();
        let ids = vec!["cl_1".to_string(), "cl_2".to_string()];
        // 2 pending * 5000ms baseline = 10000ms
        assert_eq!(tracker.eta_ms(&ids), 10000);
    }

    #[test]
    fn test_eta_computes_from_avg() {
        let tracker = JobTracker::new();
        tracker.record_elapsed(2000);
        tracker.record_elapsed(4000);
        tracker.mark_done("cl_1");
        let ids = vec!["cl_1".to_string(), "cl_2".to_string(), "cl_3".to_string()];
        // avg = (2000+4000)/2 = 3000; pending = 2; eta = 3000*2 = 6000
        assert_eq!(tracker.eta_ms(&ids), 6000);
    }

    #[test]
    fn test_eta_capped_at_60s() {
        let tracker = JobTracker::new();
        tracker.record_elapsed(50000);
        // avg = 50000; baseline (no pending calculation needed since we have history)
        // We need many pending to exceed cap
        let mut ids = Vec::new();
        for i in 0..10 {
            ids.push(format!("cl_{i}"));
        }
        // avg=50000, pending=10, uncapped=500000 → capped to 60000
        assert_eq!(tracker.eta_ms(&ids), 60000);
    }

    #[test]
    fn tracker_returns_materialized_files_for_query_jobs() {
        let tracker = JobTracker::new();
        let first = types::FileId::generate("first.c");
        let second = types::FileId::generate("second.c");
        tracker.record_built_files("job-1", [first, second]);
        tracker.record_built_files("job-2", [second]);

        let files = tracker.built_files_for(&["job-1".into(), "job-2".into()]);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&first));
        assert!(files.contains(&second));
    }

    #[test]
    fn refresh_files_are_deduplicated_drained_once_and_keep_job_history() {
        let tracker = JobTracker::new();
        let first = types::FileId::generate("first.c");
        let second = types::FileId::generate("second.c");
        tracker.record_built_files("job-1", [first, second]);
        tracker.record_built_files("job-2", [second]);

        let refresh_files = tracker.take_refresh_files();
        assert_eq!(refresh_files.len(), 2);
        assert!(refresh_files.contains(&first));
        assert!(refresh_files.contains(&second));
        assert!(tracker.take_refresh_files().is_empty());

        let job_files = tracker.built_files_for(&["job-1".into(), "job-2".into()]);
        assert_eq!(job_files.len(), 2);
        assert!(job_files.contains(&first));
        assert!(job_files.contains(&second));
    }

    #[test]
    fn failed_job_is_terminal_and_keeps_its_diagnostic() {
        let tracker = JobTracker::new();
        tracker.mark_failed("job-1", "closure build failed");

        assert!(tracker.are_all_done(&["job-1".into()]));
        assert_eq!(
            tracker.failures_for(&["job-1".into()]),
            vec![("job-1".into(), "closure build failed".into())]
        );
    }
}
