//! Incremental sync engine: detect changes, re-extract, re-resolve, reload graph.
//!
//! Change detection uses git status as the primary strategy, with a DB content-hash
//! fallback for non-git projects. Both paths compare against the single source of truth
//! in `files.content_hash`.

pub mod detector;
pub mod discovery;
pub mod file_lock;

pub use file_lock::FileLock;

#[cfg(feature = "sync")]
pub mod watcher;

use anyhow::{Context, Result};
use atlas_db::Store;
use atlas_extraction::LanguageRegistry;
use atlas_extraction::create_frontend;
use atlas_extraction::extract_file;
use atlas_graph::{GraphBuilder, GraphEngine, GraphSnapshot};
use atlas_types::{PhaseTimer, PhaseTimings};
use atlas_workspace::SourcePath;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Incremental sync engine.
pub struct SyncEngine {
    store: Arc<Store>,
    project_root: PathBuf,
}

impl SyncEngine {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root,
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

        if changed.is_empty() {
            stats.duration = start.elapsed();
            phase_timings.set_total(stats.duration);
            stats.phase_timings = phase_timings;
            return Ok(stats);
        }

        // 2. Delete stale data for deleted and modified files
        let del_timer = PhaseTimer::start("Delete stale");
        // CRITICAL: FileId must be generated from project-relative paths,
        // matching the path used during extraction (reindex_file).
        for path in &changed.deleted {
            let relative = path.strip_prefix(&self.project_root).unwrap_or(path);
            let sp = SourcePath::try_from_relative(&relative.to_string_lossy())
                .with_context(|| format!("invalid deleted path: {}", relative.display()))?;
            let file_id = atlas_types::ids::FileId::generate(sp.as_str());
            // P2: Invalidate OTHER files' references pointing to this file's
            // symbols before deleting (prevents dangling resolved targets).
            let _ = self.store.invalidate_references_to_symbols_in_file(&file_id);
            // Invalidate edges derived from this file's references before
            // deleting the file (CASCADE handles the rest).
            self.store
                .delete_edges_for_file_references(&file_id)
                .with_context(|| format!("failed to delete edges for {}", path.display()))?;
            self.store.delete_file_data(&file_id)?;
            stats.files_removed += 1;
        }

        for path in &changed.modified {
            let relative = path.strip_prefix(&self.project_root).unwrap_or(path);
            let sp = SourcePath::try_from_relative(&relative.to_string_lossy())
                .with_context(|| format!("invalid modified path: {}", relative.display()))?;
            let file_id = atlas_types::ids::FileId::generate(sp.as_str());
            // Invalidate cross-file references before clearing own data
            let _ = self.store.invalidate_references_to_symbols_in_file(&file_id);
            // Invalidate resolved facts and derived edges for modified files.
            // This ensures stale resolution targets don't persist after re-extraction.
            self.store
                .invalidate_references_for_file(&file_id)
                .with_context(|| format!("failed to invalidate refs for {}", path.display()))?;
            self.store
                .delete_edges_for_file_references(&file_id)
                .with_context(|| format!("failed to delete edges for {}", path.display()))?;
            self.store.delete_file_data(&file_id)?;
        }
        let del_count = changed.deleted.len() + changed.modified.len();
        let del_timing = del_timer.items(del_count as u64).finish();
        phase_timings.push(del_timing);

        // P2: tsconfig.json change invalidation
        // When path aliases change, all import resolutions and derived edges
        // become stale. Invalidate everything so re-resolution uses the new aliases.
        let tsconfig_was_changed = changed.tsconfig_changed;
        if tsconfig_was_changed {
            let inv_timer = PhaseTimer::start("Tsconfig invalidation");
            let inv_refs = self
                .store
                .invalidate_all_references()
                .context("Failed to invalidate references for tsconfig change")?;
            let inv_edges = self
                .store
                .delete_all_edges()
                .context("Failed to delete edges for tsconfig change")?;
            tracing::info!(
                "tsconfig.json changed — invalidated {} references and {} edges",
                inv_refs,
                inv_edges
            );
            let inv_timing = inv_timer
                .note(format!("{} refs + {} edges", inv_refs, inv_edges))
                .finish();
            phase_timings.push(inv_timing);
        }

        // 3. Re-extract modified and new files
        let ext_timer = PhaseTimer::start("Re-extract");
        let to_reindex: Vec<&PathBuf> = changed
            .added
            .iter()
            .chain(changed.modified.iter())
            .collect();

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
        let res_timer = PhaseTimer::start("Resolution");
        // P2: Load tsconfig.json or jsconfig.json path aliases if present
        let path_alias = atlas_resolution::PathAliasResolver::from_tsconfig(
            &self.project_root.join("tsconfig.json"),
        )
        .or_else(|| {
            atlas_resolution::PathAliasResolver::from_jsconfig(
                &self.project_root.join("jsconfig.json"),
            )
        })
        .unwrap_or_else(atlas_resolution::PathAliasResolver::empty);
        let mut resolver =
            atlas_resolution::ReferenceResolver::with_path_alias(self.store.clone(), path_alias);
        let (resolved, res_stats) = resolver.resolve_all()?;
        let res_timing = res_timer
            .items(res_stats.total_refs as u64)
            .note(format!("{} resolved", res_stats.resolved))
            .finish();
        phase_timings.push(res_timing);

        // 4b. Build edges from resolved references
        let edge_timer = PhaseTimer::start("Graph build");
        let builder = GraphBuilder::new(self.store.clone());
        let build_stats = builder.build_all(&resolved);
        stats.new_edges = build_stats.edges_built;
        let edge_timing = edge_timer.items(build_stats.edges_built as u64).finish();
        phase_timings.push(edge_timing);

        // Commit tsconfig hash baseline AFTER the full pipeline succeeded.
        // Committing earlier means a partial failure would leave the hash
        // updated, preventing retry on the next sync.
        if tsconfig_was_changed {
            atlas_resolution::commit_config_hashes(
                &self.store,
                &self.project_root,
                &["tsconfig.json"],
            )?;
        }

        stats.duration = start.elapsed();
        phase_timings.set_total(stats.duration);
        stats.phase_timings = phase_timings;
        Ok(stats)
    }

    /// Detect changed files: tries git status first, falls back to DB content-hash comparison.
    /// Also detects `tsconfig.json` changes for import resolution invalidation.
    pub fn detect_changes(&self) -> Result<detector::ChangedFiles> {
        // Try git first (primary strategy — fastest and most reliable)
        let mut changes = if let Some(changes) = detector::detect_git_changes(&self.project_root) {
            changes
        } else {
            // Fallback: compare current file hashes against DB-stored hashes
            detector::detect_db_hash_changes(&self.project_root, &self.store)?
        };

        // ── P2: tsconfig.json change detection ──
        changes.tsconfig_changed = self.detect_tsconfig_change()?;

        Ok(changes)
    }

    /// Check whether tsconfig.json has changed since the last sync.
    ///
    /// **Read-only** — does not write to `project_metadata`.  After the
    /// caller completes invalidation, call
    /// `atlas_resolution::commit_config_hashes` to record the new baseline.
    ///
    /// jsconfig.json is NOT checked — the resolver only loads tsconfig.json.
    /// JS projects requiring path aliases should use tsconfig.json.
    fn detect_tsconfig_change(&self) -> anyhow::Result<bool> {
        atlas_resolution::detect_config_change(&self.store, &self.project_root, &["tsconfig.json"])
    }

    // --- internal ---

    fn reindex_file(&self, path: &Path) -> Result<()> {
        let relative = path.strip_prefix(&self.project_root).unwrap_or(path);

        let lang = LanguageRegistry::detect_language(relative)
            .context("Cannot detect language for file")?;

        let frontend = create_frontend(lang).context("Language frontend not available")?;

        let source = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read {}", path.display()))?;

        let sp = SourcePath::try_from_relative(&relative.to_string_lossy())
            .with_context(|| format!("invalid file path: {}", relative.display()))?;
        let file_id = atlas_types::ids::FileId::generate(sp.as_str());
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let facts = extract_file(&frontend, file_id, relative, &source, &content_hash)?;

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
    use atlas_db::Store;
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
