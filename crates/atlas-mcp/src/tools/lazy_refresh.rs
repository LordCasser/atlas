//! LazyRefreshQueue — consolidates all graph refresh state.
//!
//! Manages pending per-file updates, cumulative write counter, and deferred
//! full-rebuild scheduling.  Uses interior mutability (Mutex, Atomic*) so
//! methods take `&self`, allowing `&mut self` ToolRouter methods to call
//! queue methods without borrow conflicts.

use std::collections::HashSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use atlas_engine::FileId;

/// Threshold for cumulative lazy file writes before scheduling a full rebuild.
const CUMULATIVE_LAZY_REBUILD_THRESHOLD: usize = 400;

/// Consolidates lazy graph refresh state: pending per-file updates, cumulative
/// write counter, and deferred full-rebuild scheduling.
pub(crate) struct LazyRefreshQueue {
    /// File IDs waiting for per-file incremental graph refresh.
    pending_file_ids: Mutex<HashSet<FileId>>,
    /// Cumulative count of unique files written (deduplication-aware;
    /// only counts files not already in pending_file_ids).
    cumulative_count: AtomicUsize,
    /// Set when cumulative count crosses threshold — next graph-backed tool
    /// call should spawn a background full-rebuild task.
    rebuild_needed: AtomicBool,
    /// Set while a background rebuild thread is active (CAS gate).
    rebuild_in_progress: AtomicBool,
}

impl LazyRefreshQueue {
    /// Create a new queue wrapped in Arc for sharing with background threads.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            pending_file_ids: Mutex::new(HashSet::new()),
            cumulative_count: AtomicUsize::new(0),
            rebuild_needed: AtomicBool::new(false),
            rebuild_in_progress: AtomicBool::new(false),
        })
    }

    /// Record file IDs written by lazy extraction.
    /// Called from both foreground MCP handlers and background preparse thread.
    pub(crate) fn record_lazy_writes(&self, file_ids: &[FileId]) {
        if file_ids.is_empty() {
            return;
        }
        // Count only files new to the pending set (deduplication-aware).
        let unique_new: usize;
        if let Ok(mut pending) = self.pending_file_ids.lock() {
            let before = pending.len();
            for fid in file_ids {
                pending.insert(*fid);
            }
            unique_new = pending.len() - before;
        } else {
            // Poisoned mutex: fall back to input length.
            unique_new = file_ids.len();
        }
        if unique_new == 0 {
            return;
        }
        let new_count = self
            .cumulative_count
            .fetch_add(unique_new, Ordering::Relaxed)
            + unique_new;
        if new_count >= CUMULATIVE_LAZY_REBUILD_THRESHOLD {
            self.schedule_full_rebuild();
        }
    }

    /// Take up to `max_files` file IDs for per-file incremental refresh.
    /// Returns the batch and removes them from pending. `max_files` should be 500
    /// (match existing REPLACE_THRESHOLD in refresh_graph_for_files).
    pub(crate) fn take_incremental_batch(&self, max_files: usize) -> Vec<FileId> {
        let mut pending = match self.pending_file_ids.lock() {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        if pending.is_empty() {
            return Vec::new();
        }
        let batch: Vec<FileId> = pending.iter().take(max_files).copied().collect();
        for fid in &batch {
            pending.remove(fid);
        }
        batch
    }

    /// Check and reset the deferred full-rebuild flag.
    /// Returns true if a rebuild was scheduled, false otherwise.
    /// Called by try_apply_or_spawn_rebuild to decide whether
    /// to spawn a background full-rebuild task.
    pub(crate) fn needs_full_rebuild(&self) -> bool {
        self.rebuild_needed.swap(false, Ordering::AcqRel)
    }

    /// Set the full-rebuild flag (called when cumulative threshold is reached).
    pub(crate) fn schedule_full_rebuild(&self) {
        self.rebuild_needed.store(true, Ordering::Release);
    }

    /// Clear the full-rebuild flag after a pending rebuild graph is applied.
    pub(crate) fn mark_rebuild_applied(&self) {
        self.rebuild_needed.store(false, Ordering::Release);
    }

    /// CAS gate: try to claim the rebuild slot for this thread.
    /// Returns true if this thread won the race and should begin building.
    pub(crate) fn try_start_rebuild(&self) -> bool {
        self.rebuild_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Release the rebuild slot (called when rebuild completes or fails).
    pub(crate) fn mark_rebuild_finished(&self) {
        self.rebuild_in_progress.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn new_queue_is_empty() {
        let q = LazyRefreshQueue::new();
        assert!(q.take_incremental_batch(500).is_empty());
        assert!(!q.needs_full_rebuild());
    }

    #[test]
    fn record_lazy_writes_stores_and_counts() {
        let q = LazyRefreshQueue::new();
        let f1 = FileId::generate("a.rs");
        let f2 = FileId::generate("b.rs");
        let f3 = FileId::generate("c.rs");

        // Write f1, f2 → count = 2
        q.record_lazy_writes(&[f1, f2]);
        assert_eq!(q.cumulative_count.load(Ordering::Relaxed), 2);

        // Write f1 again → count stays 2 (deduped)
        q.record_lazy_writes(&[f1]);
        assert_eq!(q.cumulative_count.load(Ordering::Relaxed), 2);

        // Write f1, f3 → count = 3 (only f3 is new)
        q.record_lazy_writes(&[f1, f3]);
        assert_eq!(q.cumulative_count.load(Ordering::Relaxed), 3);

        let batch = q.take_incremental_batch(500);
        assert_eq!(batch.len(), 3);
        assert!(q.take_incremental_batch(500).is_empty());
    }

    #[test]
    fn schedule_full_rebuild_is_observable() {
        let q = LazyRefreshQueue::new();
        assert!(!q.needs_full_rebuild());
        q.schedule_full_rebuild();
        assert!(q.needs_full_rebuild());
        assert!(!q.needs_full_rebuild()); // swap resets
    }

    #[test]
    fn rebuild_in_progress_is_atomic_gate() {
        let q = LazyRefreshQueue::new();
        assert!(!q.rebuild_in_progress.load(Ordering::Acquire));
        assert!(q.try_start_rebuild());
        assert!(q.rebuild_in_progress.load(Ordering::Acquire));
        assert!(!q.try_start_rebuild()); // second caller loses
        q.mark_rebuild_finished();
        assert!(!q.rebuild_in_progress.load(Ordering::Acquire));
        assert!(q.try_start_rebuild()); // gate opens again
    }

    #[test]
    fn mark_rebuild_applied_clears_flag() {
        let q = LazyRefreshQueue::new();
        q.schedule_full_rebuild();
        q.mark_rebuild_applied();
        assert!(!q.needs_full_rebuild());
    }

    #[test]
    fn cumulative_threshold_triggers_full_rebuild() {
        let q = LazyRefreshQueue::new();
        let fids: Vec<FileId> = (0..400)
            .map(|i| FileId::generate(&format!("{i}.rs")))
            .collect();
        // First 399 should NOT trigger
        q.record_lazy_writes(&fids[..399]);
        assert!(!q.needs_full_rebuild());
        // 400th should trigger (cumulative_count reaches 400)
        q.record_lazy_writes(&fids[399..]);
        assert!(q.needs_full_rebuild());
    }

    #[test]
    fn take_incremental_batch_respects_max() {
        let q = LazyRefreshQueue::new();
        let fids: Vec<FileId> = (0..10)
            .map(|i| FileId::generate(&format!("{i}.rs")))
            .collect();
        q.record_lazy_writes(&fids);
        let batch = q.take_incremental_batch(3);
        assert_eq!(batch.len(), 3);
        let remaining = q.take_incremental_batch(500);
        assert_eq!(remaining.len(), 7);
    }

    #[test]
    fn record_lazy_writes_deduplicates() {
        let q = LazyRefreshQueue::new();
        let f1 = FileId::generate("a.rs");
        // Insert same file twice
        q.record_lazy_writes(&[f1]);
        q.record_lazy_writes(&[f1]);
        let batch = q.take_incremental_batch(500);
        assert_eq!(batch.len(), 1);
    }
}
