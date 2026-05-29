//! Incremental sync engine: detect changes, re-extract, re-resolve, reload graph.
//!
//! Change detection uses git status as the primary strategy, with a DB content-hash
//! fallback for non-git projects. Both paths compare against the single source of truth
//! in `files.content_hash`.

pub mod cleanup;
pub mod detector;
pub mod dirty;
pub mod discovery;
pub mod file_lock;
pub mod index_phases;
pub mod index_pipeline;
pub mod sync_engine;

pub use cleanup::{clean_stale_file_ids, clean_stale_file_paths, source_file_id};
pub use dirty::{DirtySet, build_dirty_set};
pub use file_lock::FileLock;
pub use index_phases::{
    phase_build_summaries, phase_cleanup_file_ids, phase_cleanup_stale,
    phase_commit_path_alias_config, phase_dirty_check, phase_discover, phase_extract_serial,
    phase_finalize, phase_init_frontends, phase_materialize_annotations,
    phase_resolve_and_build, phase_write_batched, phase_write_single, ExtractedFile,
    ExtractedFiles, ExtractionPhaseStats, GraphResult, WriteBatchStats,
};
pub use index_pipeline::{
    run_index_pipeline, IndexPipelineOptions, IndexPipelineStats, IndexProgress,
    IndexProgressCallback,
};
pub use sync_engine::{load_graph, load_snapshot, SyncEngine, SyncStats};

#[cfg(feature = "sync")]
pub mod watcher;
