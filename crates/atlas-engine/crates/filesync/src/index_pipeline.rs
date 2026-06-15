//! Shared indexing pipeline used by higher-level entry points.
//!
//! This module owns the reusable indexing mechanics: discovery, stale fact
//! cleanup, extraction, optional reference resolution, and graph edge build.
//! Callers remain responsible for UI, background execution, locking, and
//! choosing the extraction mode.
//!
//! Internally the pipeline delegates to the composable phase functions in
//! [`crate::index_phases`].

use std::path::Path;
use std::sync::Arc;

use db::Store;
use extraction::ExtractionMode;

use crate::index_pipeline_orchestrator::IndexPipeline;
use crate::progress::{CallbackSink, NoopSink, ProgressSink};

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
/// Per-phase wall-clock timing, in milliseconds.
///
/// All fields default to 0 — phases are filled in as the pipeline executes.
#[derive(Debug, Default, Clone, Copy)]
pub struct PipelinePhaseTiming {
    pub discovery_ms: u64,
    pub hash_check_ms: u64,
    pub cleanup_ms: u64,
    pub language_init_ms: u64,
    pub extraction_ms: u64,
    pub db_write_ms: u64,
    pub resolution_graph_ms: u64,
    pub annotation_ms: u64,
    pub summary_build_ms: u64,
    pub finalize_ms: u64,
}

impl PipelinePhaseTiming {
    /// Total wall-clock time across all phases, in ms.
    pub fn total_ms(&self) -> u64 {
        self.discovery_ms
            + self.hash_check_ms
            + self.cleanup_ms
            + self.language_init_ms
            + self.extraction_ms
            + self.db_write_ms
            + self.resolution_graph_ms
            + self.annotation_ms
            + self.summary_build_ms
            + self.finalize_ms
    }
}

#[derive(Debug, Clone, Default)]
pub struct IndexPipelineStats {
    pub discovered: usize,
    pub indexed: usize,
    pub failed: usize,
    pub symbols: usize,
    pub resolved: usize,
    pub edges_built: usize,
    /// Number of schema objects (indexes/triggers) restored by
    /// [`Store::ensure_required_schema_objects`] during finalization.
    pub schema_repaired: usize,
    /// Per-phase wall-clock timing breakdown.
    pub phases: PipelinePhaseTiming,
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
    // Route progress through the new ProgressSink / CallbackSink mechanism,
    // then delegate entirely to IndexPipeline::run.
    let sink: Box<dyn ProgressSink> = match &options.progress {
        Some(cb) => Box::new(CallbackSink::new(Arc::clone(cb))),
        None => Box::new(NoopSink),
    };

    let pipeline = IndexPipeline::new(
        Arc::clone(store),
        project_root.to_path_buf(),
        IndexPipelineOptions {
            mode: options.mode,
            include_patterns: options.include_patterns,
            exclude_patterns: options.exclude_patterns,
            progress: None,
        },
    );

    pipeline.run(&*sink, &mut || false)
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
