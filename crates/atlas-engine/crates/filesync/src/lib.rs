//! Incremental sync engine: detect changes, re-extract, re-resolve, reload graph.
//!
//! Change detection compares the current source inventory with the persisted
//! `files.content_hash` baseline. Git state is intentionally irrelevant: HEAD
//! is not the same authority as the last successful Atlas index or sync.

pub mod cleanup;
pub mod detector;
pub mod dirty;
pub mod discovery;
pub mod file_lock;
pub mod incremental_pipeline;
pub mod index_phases;
pub mod index_pipeline;
pub mod index_pipeline_orchestrator;
pub mod progress;
pub mod sync_engine;

pub use cleanup::{clean_stale_file_ids, clean_stale_file_paths, source_file_id};
pub use detector::ChangedFiles;
pub use dirty::{DirtySet, build_dirty_set};
pub use file_lock::{FileLock, IndexLockHeld};
pub use incremental_pipeline::IncrementalPipeline;
pub use index_phases::{
    ExtractedFile, ExtractedFiles, ExtractionPhaseStats, GraphResult, WriteBatchStats,
    phase_build_summaries, phase_cleanup_file_ids, phase_cleanup_stale,
    phase_commit_path_alias_config, phase_dirty_check, phase_discover, phase_extract_serial,
    phase_finalize, phase_init_frontends, phase_materialize_annotations, phase_resolve_and_build,
    phase_write_batched, phase_write_single,
};
pub use index_pipeline::{IndexPipelineOptions, IndexPipelineStats, run_index_pipeline};
pub use index_pipeline_orchestrator::IndexPipeline;
pub use progress::{MultiplexSink, NoopSink, PhaseName, ProgressEvent, ProgressSink};
pub use sync_engine::{SyncEngine, SyncStats, load_graph, load_snapshot};

#[cfg(feature = "sync")]
pub mod watcher;
