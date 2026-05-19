//! Incremental sync engine: detect changes, re-extract, re-resolve, reload graph.

pub mod detector;
pub mod discovery;

#[cfg(feature = "sync")]
pub mod watcher;

use crate::db::Store;
use crate::extraction::create_adapter;
use crate::extraction::extract_file;
use crate::extraction::LanguageRegistry;
use crate::graph::{GraphEngine, GraphSnapshot};
use crate::resolution::ReferenceResolver;
use anyhow::{Context, Result};
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
    /// 1. Detect changed files (git status → content-hash fallback)
    /// 2. Delete stale data for removed/changed files
    /// 3. Re-extract changed/added files
    /// 4. Re-resolve all unresolved references
    /// 5. Persist file hashes for next sync
    pub fn sync(&self) -> Result<SyncStats> {
        let start = Instant::now();

        // 1. Detect changes (with hash store persistence)
        let (changed, hash_store) = self.detect_changes_with_hash()?;
        let mut stats = SyncStats {
            files_changed: changed.total(),
            ..Default::default()
        };

        if changed.is_empty() {
            stats.duration = start.elapsed();
            return Ok(stats);
        }

        // 2. Delete stale data for deleted and modified files
        // CRITICAL: FileId must be generated from project-relative paths,
        // matching the path used during extraction (reindex_file).
        for path in &changed.deleted {
            let relative = path
                .strip_prefix(&self.project_root)
                .unwrap_or(path);
            let file_id = crate::types::ids::FileId::generate(
                &relative.to_string_lossy(),
            );
            self.store.delete_file_data(&file_id)?;
            stats.files_removed += 1;
        }

        for path in &changed.modified {
            let relative = path
                .strip_prefix(&self.project_root)
                .unwrap_or(path);
            let file_id =
                crate::types::ids::FileId::generate(&relative.to_string_lossy());
            self.store.delete_file_data(&file_id)?;
        }

        // 3. Re-extract modified and new files
        let to_reindex: Vec<&PathBuf> = changed
            .added
            .iter()
            .chain(changed.modified.iter())
            .collect();

        let before_symbols = self.store.count_symbols().unwrap_or(0);

        for path in &to_reindex {
            if let Err(e) = self.reindex_file(path) {
                tracing::warn!("Failed to reindex {}: {}", path.display(), e);
            } else {
                stats.files_reindexed += 1;
            }
        }

        let after_symbols = self.store.count_symbols().unwrap_or(0);
        stats.new_nodes = after_symbols.saturating_sub(before_symbols);

        // 4. Re-resolve all unresolved references
        let resolver = ReferenceResolver::new(self.store.clone());
        let res_stats = resolver.resolve_all()?;
        stats.new_edges = res_stats.resolved;

        // 5. Persist file hashes for the next incremental sync
        let atlas_dir = self.project_root.join(".atlas");
        std::fs::create_dir_all(&atlas_dir).ok();
        hash_store.save(&atlas_dir)?;

        stats.duration = start.elapsed();
        Ok(stats)
    }

    /// Reload the GraphSnapshot from the store (after sync completes).
    pub fn reload_graph(&self, confidence_threshold: f32) -> Result<GraphEngine> {
        GraphEngine::from_store(&self.store, confidence_threshold)
    }

    /// Detect changed files: tries git status first, falls back to content-hash comparison.
    /// Returns changes only (backward-compatible wrapper).
    pub fn detect_changes(&self) -> Result<detector::ChangedFiles> {
        self.detect_changes_with_hash().map(|(changes, _)| changes)
    }

    /// Detect changed files: tries git status first, falls back to content-hash comparison.
    /// Returns changes + the hash store for persistence after sync completes.
    pub fn detect_changes_with_hash(
        &self,
    ) -> Result<(detector::ChangedFiles, detector::FileHashStore)> {
        let atlas_dir = self.project_root.join(".atlas");
        std::fs::create_dir_all(&atlas_dir).ok();

        // Try git first (primary strategy — fastest and most reliable)
        if let Some(changes) = detector::detect_git_changes(&self.project_root) {
            return Ok((changes, detector::FileHashStore::default()));
        }

        // Fallback: content-hash comparison using .atlas/file_hashes.json
        let mut hash_store = detector::FileHashStore::load(&atlas_dir)?;
        let changes = detector::detect_hash_changes(&self.project_root, &mut hash_store)?;
        Ok((changes, hash_store))
    }

    // --- internal ---

    fn reindex_file(&self, path: &Path) -> Result<()> {
        let relative = path
            .strip_prefix(&self.project_root)
            .unwrap_or(path);

        let lang = LanguageRegistry::detect_language(relative)
            .context("Cannot detect language for file")?;

        let adapter = create_adapter(lang).context("Language adapter not available")?;

        let source = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read {}", path.display()))?;

        let file_id = crate::types::ids::FileId::generate(&relative.to_string_lossy());
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let facts = extract_file(
            adapter.as_ref(),
            file_id,
            relative,
            &source,
            &content_hash,
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

/// Statistics from a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub files_changed: usize,
    pub files_reindexed: usize,
    pub files_removed: usize,
    pub new_nodes: usize,
    pub new_edges: usize,
    pub duration: std::time::Duration,
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Store;
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
