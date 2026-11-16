//! Shared indexing pipeline used by higher-level entry points.
//!
//! This module owns the reusable indexing mechanics: discovery, stale fact
//! cleanup, extraction, optional reference resolution, and graph edge build.
//! Callers remain responsible for UI, background execution, locking, and
//! choosing the extraction mode.
//!
//! Internally the pipeline delegates to the composable phase functions in
//! [`crate::index_phases`].

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use db::Store;
use extraction::ExtractionMode;

use crate::index_pipeline_orchestrator::IndexPipeline;
use crate::progress::{NoopSink, ProgressSink};

pub(crate) const KEY_FILE_LANGUAGES: &str = "file_language_overrides";
pub(crate) const KEY_COMPILER_CALL_INPUT: &str = "compiler_call_input_hash";
/// Located limitations from the explicitly selected compiler call input.
pub const KEY_COMPILER_CALL_GAPS: &str = "compiler_call_gaps";

/// Options controlling one index pipeline run.
#[derive(Clone)]
pub struct IndexPipelineOptions {
    pub mode: ExtractionMode,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    /// Explicit language for selected project-relative files (e.g. C++ `.h`).
    /// This is analysis input, not an inference from repository names.
    pub file_languages: BTreeMap<PathBuf, types::Language>,
    /// Ordered, project-relative C/C++ header search directories. Includes
    /// resolve only against indexed inputs; this does not expand discovery.
    pub include_paths: Vec<PathBuf>,
    /// Explicit compiler observations, merged before graph construction. Never
    /// discovered automatically from repository files. None selects source-only.
    pub compiler_calls: Option<resolution::compiler::CompilerCallInput>,
}

impl IndexPipelineOptions {
    pub fn new(mode: ExtractionMode) -> Self {
        Self {
            mode,
            include_patterns: Vec::new(),
            exclude_patterns: Vec::new(),
            file_languages: BTreeMap::new(),
            include_paths: Vec::new(),
            compiler_calls: None,
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

    pub fn with_file_languages(
        mut self,
        file_languages: BTreeMap<PathBuf, types::Language>,
    ) -> Self {
        self.file_languages = file_languages;
        self
    }

    pub fn with_include_paths(mut self, include_paths: Vec<PathBuf>) -> Self {
        self.include_paths = include_paths;
        self
    }

    pub fn with_compiler_calls(mut self, input: resolution::compiler::CompilerCallInput) -> Self {
        self.compiler_calls = Some(input);
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
    let sink: Box<dyn ProgressSink> = Box::new(NoopSink);

    let pipeline = IndexPipeline::new(Arc::clone(store), project_root.to_path_buf(), options);

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
        assert_eq!(
            store.get_metadata("indexed_pipeline_grade").unwrap(),
            Some("manifest".into())
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &store.get_metadata("indexed_scope").unwrap().unwrap()
            )
            .unwrap(),
            serde_json::json!({ "include": [], "exclude": [] })
        );
    }
}
