//! Shared indexing pipeline used by higher-level entry points.
//!
//! This module owns the reusable indexing mechanics: discovery, stale fact
//! cleanup, extraction, optional reference resolution, and graph edge build.
//! Callers remain responsible for UI, background execution, locking, and
//! choosing the extraction mode.
//!
//! Internally the pipeline delegates to the composable phase functions in
//! [`crate::index_phases`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use db::Store;
use extraction::ExtractionMode;

use crate::index_phases::{
    phase_build_summaries, phase_cleanup_stale, phase_discover, phase_extract_serial,
    phase_init_frontends, phase_materialize_annotations, phase_resolve_and_build,
};

/// Progress callback payload emitted by [`run_index_pipeline`].
#[derive(Debug, Clone)]
pub struct IndexProgress {
    /// Fraction in the range 0.0..=1.0.
    pub fraction: f64,
    /// Optional total for clients that support progress totals.
    pub total: Option<f64>,
    /// Human-readable phase message.
    pub message: Option<String>,
}

/// Callback type for index progress events.
pub type IndexProgressCallback = Arc<dyn Fn(IndexProgress) + Send + Sync>;

/// Options controlling one index pipeline run.
#[derive(Clone)]
pub struct IndexPipelineOptions {
    pub mode: ExtractionMode,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub progress: Option<IndexProgressCallback>,
}

impl IndexPipelineOptions {
    pub fn new(mode: ExtractionMode) -> Self {
        Self {
            mode,
            include_patterns: Vec::new(),
            exclude_patterns: Vec::new(),
            progress: None,
        }
    }

    pub fn with_include_patterns(mut self, include_patterns: Vec<String>) -> Self {
        self.include_patterns = include_patterns;
        self
    }

    pub fn with_exclude_patterns(mut self, exclude_patterns: Vec<String>) -> Self {
        self.exclude_patterns = exclude_patterns;
        self
    }

    pub fn with_progress(mut self, progress: IndexProgressCallback) -> Self {
        self.progress = Some(progress);
        self
    }
}

/// Statistics from one index pipeline run.
#[derive(Debug, Clone, Default)]
pub struct IndexPipelineStats {
    pub discovered: usize,
    pub indexed: usize,
    pub failed: usize,
    pub symbols: usize,
    pub resolved: usize,
    pub edges_built: usize,
}

/// Run the shared index pipeline against `project_root`.
///
/// The caller is responsible for cross-process locking when the store is
/// persistent. `Manifest` mode stops after extraction; `Structural` and `Full`
/// additionally run reference resolution and graph edge building.
pub fn run_index_pipeline(
    store: &Arc<Store>,
    project_root: &Path,
    options: IndexPipelineOptions,
) -> anyhow::Result<IndexPipelineStats> {
    // ── Phase 1: Discover ──
    let discovered = phase_discover(
        project_root,
        &options.include_patterns,
        &options.exclude_patterns,
    )?;
    if discovered.is_empty() {
        return Ok(IndexPipelineStats::default());
    }

    emit(
        &options,
        0.10,
        Some(1.0),
        format!(
            "Discovered {} files, starting extraction...",
            discovered.len()
        ),
    );

    // ── Clean up stale files deleted from disk since last index ──
    let db_file_paths: Vec<PathBuf> = store
        .list_files()
        .unwrap_or_default()
        .into_iter()
        .map(|f| PathBuf::from(f.path))
        .collect();
    let discovered_set: HashSet<&PathBuf> = discovered.iter().collect();
    let deleted: Vec<PathBuf> = db_file_paths
        .into_iter()
        .filter(|p| !discovered_set.contains(p))
        .collect();
    if !deleted.is_empty() {
        phase_cleanup_stale(store, &deleted)?;
    }

    // ── Phase 3: Init frontends + clean stale ──
    let frontend_cache = phase_init_frontends(&discovered)?;
    phase_cleanup_stale(store, &discovered)?;

    // ── Phase 5: Extract (with progress) ──
    let total_files = discovered.len() as f64;
    let processed = std::cell::Cell::new(0usize);

    let extract_progress: &dyn Fn(usize, usize) = &|current, _total| {
        let prev = processed.get();
        if current > prev {
            processed.set(current);
            if prev % 50 == 0 || current == discovered.len() {
                let fraction = 0.10 + 0.50 * current as f64 / total_files.max(1.0);
                emit(
                    &options,
                    fraction.min(0.60),
                    Some(1.0),
                    format!("Extracting files... {}/{}", current, discovered.len()),
                );
            }
        }
    };

    let extracted = phase_extract_serial(
        project_root,
        &discovered,
        &frontend_cache,
        options.mode.clone(),
        Some(extract_progress),
    );

    // ── Phase 6a: Write facts one-at-a-time ──
    let mut stats = IndexPipelineStats {
        discovered: discovered.len(),
        ..Default::default()
    };
    for file in &extracted.items {
        match store.insert_file_facts(&file.facts) {
            Ok(_) => stats.indexed += 1,
            Err(e) => {
                stats.failed += 1;
                tracing::warn!("Insert failed for {}: {:#}", file.rel_path.display(), e);
            }
        }
    }
    stats.failed += extracted.stats.failed;
    stats.symbols = extracted.stats.symbols;

    emit(
        &options,
        0.65,
        Some(1.0),
        format!(
            "Extraction complete: {} indexed, {} failed ({} symbols found)",
            stats.indexed, stats.failed, stats.symbols
        ),
    );

    // ── Manifest mode: stop here ──
    if matches!(options.mode, ExtractionMode::Manifest) {
        emit(
            &options,
            1.0,
            Some(1.0),
            format!(
                "Manifest indexing complete: {} files indexed ({} failed), {} symbols",
                stats.indexed, stats.failed, stats.symbols
            ),
        );
        return Ok(stats);
    }

    // ── Phase 7: Resolve + build graph ──
    emit(
        &options,
        0.75,
        Some(1.0),
        "Resolving symbol references...".to_string(),
    );

    let graph_result = phase_resolve_and_build(store, project_root)?;
    stats.resolved = graph_result.resolved;
    stats.edges_built = graph_result.edges_built;

    emit(
        &options,
        0.90,
        Some(1.0),
        "Building symbol graph...".to_string(),
    );

    // ── Phase 8: Materialize annotations ──
    if let Err(e) = phase_materialize_annotations(store) {
        tracing::warn!("Failed to materialize annotations: {:#}", e);
    }

    // ── Phase 9: Build summaries (Full mode only) ──
    if options.mode.produces_dataflow() {
        if let Err(e) = phase_build_summaries(store) {
            tracing::warn!("Failed to build summaries: {:#}", e);
        }
    }

    emit(
        &options,
        1.0,
        Some(1.0),
        format!(
            "Indexing complete: {} files indexed ({} failed), {} symbols, {} resolved",
            stats.indexed, stats.failed, stats.symbols, stats.resolved
        ),
    );
    Ok(stats)
}

// ── Private helpers ────────────────────────────────────────────────────

fn emit(options: &IndexPipelineOptions, fraction: f64, total: Option<f64>, message: String) {
    if let Some(progress) = &options.progress {
        progress(IndexProgress {
            fraction,
            total,
            message: Some(message),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_pipeline_indexes_symbols_without_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("main.ts");
        std::fs::write(
            &file_path,
            "export function greet(name: string) { return `hi ${name}`; }\n",
        )
        .unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let stats = run_index_pipeline(
            &store,
            dir.path(),
            IndexPipelineOptions::new(ExtractionMode::Manifest),
        )
        .unwrap();

        assert_eq!(stats.discovered, 1);
        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.failed, 0);
        assert!(stats.symbols > 0);
        assert_eq!(stats.resolved, 0);
        assert!(store.count_symbols().unwrap() > 0);
    }
}
