//! Sync engine implementation: incremental change detection, stale-fact
//! cleanup, re-extraction, reference resolution, and graph edge building.
//!
//! `SyncEngine` is the primary entry point for incremental index updates.
//! For full-index pipelines, prefer [`crate::index_pipeline::run_index_pipeline`].

use anyhow::Result;
use db::Store;
use extraction::ExtractionMode;
use graph::{GraphEngine, GraphSnapshot};
use std::path::PathBuf;
use std::sync::Arc;
use types::PhaseTimings;

use crate::FileLock;
use crate::incremental_pipeline::IncrementalPipeline;
use crate::progress::ProgressSink;

/// Incremental sync engine.
pub struct SyncEngine {
    store: Arc<Store>,
    project_root: PathBuf,
    mode: ExtractionMode,
}

impl SyncEngine {
    /// Create a SyncEngine with the default extraction mode (Structural).
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root,
            mode: ExtractionMode::Structural,
        }
    }

    /// Create a SyncEngine with a specific extraction mode.
    pub fn with_mode(store: Arc<Store>, project_root: PathBuf, mode: ExtractionMode) -> Self {
        Self {
            store,
            project_root,
            mode,
        }
    }

    /// Perform incremental sync while emitting structured progress events.
    pub fn sync(
        &self,
        sink: &dyn ProgressSink,
        interrupted: &mut dyn FnMut() -> bool,
    ) -> Result<SyncStats> {
        let _lock = FileLock::acquire(&self.store)?;
        let pipeline = IncrementalPipeline::new(
            Arc::clone(&self.store),
            self.project_root.clone(),
            self.mode.clone(),
        );
        pipeline.sync(sink, interrupted)
    }

    /// Detect changed files: tries git status first, falls back to DB content-hash comparison.
    pub fn detect_changes(&self) -> Result<crate::detector::ChangedFiles> {
        // Try git first (primary strategy — fastest and most reliable)
        if let Some(changes) = crate::detector::detect_git_changes(&self.project_root) {
            Ok(changes)
        } else {
            // Fallback: compare current file hashes against DB-stored hashes
            crate::detector::detect_db_hash_changes(&self.project_root, &self.store)
        }
    }
}

// -----------------------------------------------------------------------
// Re-export GraphEngine for convenience
// -----------------------------------------------------------------------

/// Create a new GraphEngine from the store (convenience for sync users).
pub fn load_graph(store: &Arc<Store>, confidence_threshold: f32) -> Result<GraphEngine> {
    GraphEngine::from_store(store, confidence_threshold)
}

/// Create a new GraphSnapshot from the store.
pub fn load_snapshot(store: &Arc<Store>, confidence_threshold: f32) -> Result<GraphSnapshot> {
    GraphSnapshot::from_store(store, confidence_threshold)
}

/// Statistics from a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub files_changed: usize,
    pub files_reindexed: usize,
    pub files_removed: usize,
    pub new_nodes: usize,
    pub new_edges: usize,
    pub summaries_updated: usize,
    pub summaries_skipped: usize,
    pub duration: std::time::Duration,
    /// Per-phase timing breakdown (P0: performance observability).
    pub phase_timings: PhaseTimings,
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use std::sync::Arc;

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    #[test]
    fn test_sync_engine_creation() {
        let store = test_store();
        let engine = SyncEngine::new(store, PathBuf::from("."));
        assert_eq!(engine.project_root, PathBuf::from("."));
    }

    #[test]
    fn test_sync_stats_default() {
        let stats = SyncStats::default();
        assert_eq!(stats.files_changed, 0);
        assert_eq!(stats.files_reindexed, 0);
        assert_eq!(stats.files_removed, 0);
        assert_eq!(stats.new_nodes, 0);
        assert_eq!(stats.new_edges, 0);
        assert_eq!(stats.summaries_updated, 0);
        assert_eq!(stats.summaries_skipped, 0);
    }

    #[test]
    fn test_load_graph_empty_store() {
        let store = test_store();
        let engine = load_graph(&store, 0.0).unwrap();
        assert_eq!(engine.node_count(), 0);
        assert_eq!(engine.edge_count(), 0);
    }

    #[test]
    fn test_load_snapshot_empty_store() {
        let store = test_store();
        let snap = load_snapshot(&store, 0.0).unwrap();
        assert_eq!(snap.node_count(), 0);
    }
}
