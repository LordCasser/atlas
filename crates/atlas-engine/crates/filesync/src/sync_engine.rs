//! Sync engine implementation: incremental change detection, stale-fact
//! cleanup, re-extraction, reference resolution, and graph edge building.
//!
//! `SyncEngine` is the primary entry point for incremental index updates.
//! For full-index pipelines, prefer [`crate::index_pipeline::run_index_pipeline`].

use anyhow::{Context, Result};
use db::Store;
use extraction::ExtractionMode;
use extraction::LanguageRegistry;
use extraction::create_frontend;
use extraction::extract_file_with_mode;
use graph::{GraphBuilder, GraphEngine, GraphSnapshot};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use types::{PhaseTimer, PhaseTimings};

use crate::cleanup::{clean_stale_file_paths, source_file_id};

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

    /// Perform a full incremental sync:
    /// 1. Detect changed files (git status → DB content-hash fallback)
    /// 2. Delete stale data for removed/changed files
    /// 3. Re-extract changed/added files
    /// 4. Re-resolve all unresolved references
    pub fn sync(&self) -> Result<SyncStats> {
        let start = Instant::now();
        let mut phase_timings = PhaseTimings::new();

        // 1. Detect changes
        let det_timer = PhaseTimer::start("Detection");
        let changed = self.detect_changes()?;
        let mut stats = SyncStats {
            files_changed: changed.total(),
            ..Default::default()
        };
        let det_timing = det_timer
            .items(changed.total() as u64)
            .note(format!(
                "{} added, {} modified, {} deleted",
                changed.added.len(),
                changed.modified.len(),
                changed.deleted.len()
            ))
            .finish();
        phase_timings.push(det_timing);

        // P2: path alias config change detection (independent of file changes)
        let path_alias_config_changed =
            resolution::PathAliasConfig::has_changed(&self.store, &self.project_root)?;

        // If neither file changes nor config changes, nothing to do.
        if changed.is_empty() && !path_alias_config_changed {
            stats.duration = start.elapsed();
            phase_timings.set_total(stats.duration);
            stats.phase_timings = phase_timings;
            return Ok(stats);
        }

        // 2. Delete stale data for deleted and modified files (only when files changed)
        if !changed.is_empty() {
            tracing::info!("cleaning stale index data");
            let del_timer = PhaseTimer::start("Delete stale");
            let deleted_rel = project_relative_paths(&changed.deleted, &self.project_root);
            let modified_rel = project_relative_paths(&changed.modified, &self.project_root);
            clean_stale_file_paths(&self.store, &deleted_rel)
                .context("failed to clean deleted files")?;
            clean_stale_file_paths(&self.store, &modified_rel)
                .context("failed to clean modified files")?;
            stats.files_removed += changed.deleted.len();
            let del_count = changed.deleted.len() + changed.modified.len();
            let del_timing = del_timer.items(del_count as u64).finish();
            phase_timings.push(del_timing);
        }

        // 3. Path alias config change invalidation (independent of file changes)
        if path_alias_config_changed {
            let inv_timer = PhaseTimer::start("Path alias invalidation");
            let inv_refs = self
                .store
                .invalidate_all_references()
                .context("Failed to invalidate references for path alias config change")?;
            let inv_edges = self
                .store
                .delete_all_edges()
                .context("Failed to delete edges for path alias config change")?;
            tracing::info!(
                "path alias config changed — invalidated {} references and {} edges",
                inv_refs,
                inv_edges
            );
            let inv_timing = inv_timer
                .note(format!("{} refs + {} edges", inv_refs, inv_edges))
                .finish();
            phase_timings.push(inv_timing);
        }

        // 4. Re-extract modified and new files
        let ext_timer = PhaseTimer::start("Re-extract");
        let to_reindex: Vec<&PathBuf> = changed
            .added
            .iter()
            .chain(changed.modified.iter())
            .collect();

        tracing::info!("re-extracting {} files", to_reindex.len());

        let before_symbols = self
            .store
            .count_symbols()
            .context("failed to count symbols before re-extract")?;

        for path in &to_reindex {
            if let Err(e) = self.reindex_file(path) {
                tracing::warn!("Failed to reindex {}: {}", path.display(), e);
            } else {
                stats.files_reindexed += 1;
            }
        }

        let after_symbols = self
            .store
            .count_symbols()
            .context("failed to count symbols after re-extract")?;
        stats.new_nodes = after_symbols.saturating_sub(before_symbols);
        let ext_timing = ext_timer.items(stats.files_reindexed as u64).finish();
        phase_timings.push(ext_timing);

        // 4. Re-resolve all unresolved references (P2: two-step pipeline)
        tracing::info!("resolving symbol references");
        let res_timer = PhaseTimer::start("Resolution");
        // P2: Load path aliases if present
        let path_alias = resolution::PathAliasConfig::resolver(&self.project_root);
        let mut resolver =
            resolution::ReferenceResolver::with_path_alias(self.store.clone(), path_alias);
        let (resolved, res_stats) = resolver.resolve_all()?;
        let res_timing = res_timer
            .items(res_stats.total_refs as u64)
            .note(format!("{} resolved", res_stats.resolved))
            .finish();
        phase_timings.push(res_timing);

        // 4b. Build edges from resolved references
        tracing::info!("building symbol graph");
        let edge_timer = PhaseTimer::start("Graph build");
        let builder = GraphBuilder::new(self.store.clone());
        let build_stats = builder.build_all(&resolved);
        stats.new_edges = build_stats.edges_built;
        let edge_timing = edge_timer.items(build_stats.edges_built as u64).finish();
        phase_timings.push(edge_timing);

        // 4c. Materialize user annotations as edges
        if let Err(e) = graph::materialize_annotations(&self.store) {
            tracing::warn!("failed to materialize annotations: {}", e);
        }

        // Commit path alias config hash baseline AFTER the full pipeline succeeded.
        // Committing earlier means a partial failure would leave the hash
        // updated, preventing retry on the next sync.
        if path_alias_config_changed {
            resolution::PathAliasConfig::commit(&self.store, &self.project_root)?;
        }

        stats.duration = start.elapsed();
        phase_timings.set_total(stats.duration);
        stats.phase_timings = phase_timings;
        Ok(stats)
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

    // --- internal ---

    fn reindex_file(&self, path: &Path) -> Result<()> {
        let relative = path.strip_prefix(&self.project_root).unwrap_or(path);

        let lang = LanguageRegistry::detect_language(relative)
            .context("Cannot detect language for file")?;

        let frontend = create_frontend(lang).context("Language frontend not available")?;

        let source = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read {}", path.display()))?;

        let file_id = source_file_id(relative)
            .with_context(|| format!("invalid file path: {}", relative.display()))?;
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            relative,
            &source,
            &content_hash,
            self.mode.clone(),
        )?;

        self.store.insert_file_facts(&facts)?;
        Ok(())
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

fn project_relative_paths(paths: &[PathBuf], root: &Path) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|path| path.strip_prefix(root).unwrap_or(path).to_path_buf())
        .collect()
}

/// Statistics from a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub files_changed: usize,
    pub files_reindexed: usize,
    pub files_removed: usize,
    pub new_nodes: usize,
    pub new_edges: usize,
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
