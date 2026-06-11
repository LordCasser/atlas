//! Structured index pipeline orchestrator with progress reporting.
//!
//! [`IndexPipeline`] wraps the composable phase functions from
//! [`crate::index_phases`] and drives them with [`ProgressSink`] events
//! at every phase boundary.  It replaces the monolithic
//! [`crate::index_pipeline::run_index_pipeline`] with an object that
//! supports interruption and fine-grained progress.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use db::Store;
#[cfg(test)]
use extraction::ExtractionMode;
use tracing::debug_span;
use types::SymbolKind;

use crate::cleanup::source_file_id;
use crate::index_phases::{
    phase_build_summaries, phase_cleanup_file_ids, phase_cleanup_stale,
    phase_commit_path_alias_config, phase_dirty_check, phase_discover,
    phase_extract_parallel_cancellable, phase_finalize, phase_init_frontends,
    phase_materialize_annotations, phase_resolve_and_build, phase_write_batched,
};
use crate::index_pipeline::{IndexPipelineOptions, IndexPipelineStats};
use crate::progress::{PhaseName, ProgressEvent, ProgressSink};

/// Structured orchestrator for an index pipeline run.
///
/// Drives the full 10-phase pipeline lifecycle (discovery → extraction →
/// resolution → finalization) with interruption support and per-phase
/// progress events delivered through a [`ProgressSink`].
pub struct IndexPipeline {
    store: Arc<Store>,
    project_root: PathBuf,
    options: IndexPipelineOptions,
}

impl IndexPipeline {
    /// Create a new orchestrator bound to a store and project root.
    pub fn new(store: Arc<Store>, project_root: PathBuf, options: IndexPipelineOptions) -> Self {
        Self {
            store,
            project_root,
            options,
        }
    }

    /// Execute all enabled pipeline phases, emitting progress via `sink` and
    /// respecting `interrupted` for early cancellation.
    ///
    /// Returns aggregated indexing statistics.  Cancellation (when
    /// `interrupted()` returns `true` before a phase starts) is not an error
    /// — it returns `Ok(IndexPipelineStats::default())` with a
    /// [`ProgressEvent::Cancelled`] event.
    pub fn run(
        &self,
        sink: &dyn ProgressSink,
        interrupted: &mut (dyn FnMut() -> bool + Send),
    ) -> Result<IndexPipelineStats> {
        let mut stats = IndexPipelineStats::default();
        // Initial value: the first phase we'd attempt.  Updated to the last
        // *completed* phase after each success.  Used as `last_phase` in
        // Cancelled events so consumers know how far we got.
        let mut last_phase = PhaseName::Discovery;

        // Interrupt callable must be shared between inline checks, the
        // on_file_progress callback (which runs inside rayon), and the
        // closure we hand to `phase_write_batched`.  Wrap in a Mutex so
        // we can invoke it from multiple threads safely.
        let int_cell = std::sync::Arc::new(std::sync::Mutex::new(interrupted));

        // Convenience: check for cancellation before the next phase.
        macro_rules! check_cancelled {
            () => {
                if (*int_cell.lock().expect("cancellation check lock poisoned"))() {
                    sink.emit(ProgressEvent::Cancelled { last_phase });
                    return Ok(IndexPipelineStats::default());
                }
            };
        }

        // ── Phase 1: Discovery ──────────────────────────────────────────
        check_cancelled!();
        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::Discovery,
            total: 0,
        });
        let discovered = match phase_discover(
            &self.project_root,
            &self.options.include_patterns,
            &self.options.exclude_patterns,
        ) {
            Ok(files) => files,
            Err(e) => {
                sink.emit(ProgressEvent::Warning {
                    phase: PhaseName::Discovery,
                    message: format!("{e:#}"),
                });
                return Err(e);
            }
        };
        stats.discovered = discovered.len();
        sink.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::Discovery,
            succeeded: discovered.len() as u64,
            failed: 0,
            detail: Some(format!("{} files discovered", discovered.len())),
        });
        last_phase = PhaseName::Discovery;

        if discovered.is_empty() {
            return Ok(stats);
        }

        // ── Phase 2: HashCheck ──────────────────────────────────────────
        check_cancelled!();
        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::HashCheck,
            total: discovered.len() as u64,
        });

        let on_hash_progress = |completed: u64| {
            sink.emit(ProgressEvent::ItemProgress {
                phase: PhaseName::HashCheck,
                completed,
            });
        };

        let dirty_set = match phase_dirty_check(
            &self.store,
            &discovered,
            &self.project_root,
            &self.options.mode,
            Some(&on_hash_progress),
        ) {
            Ok(ds) => ds,
            Err(e) => {
                sink.emit(ProgressEvent::Warning {
                    phase: PhaseName::HashCheck,
                    message: format!("{e:#}"),
                });
                return Err(e);
            }
        };
        sink.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::HashCheck,
            succeeded: (dirty_set.dirty.len() + dirty_set.clean_count) as u64,
            failed: 0,
            detail: Some(format!(
                "{} dirty, {} clean, {} deleted",
                dirty_set.dirty.len(),
                dirty_set.clean_count,
                dirty_set.deleted.len(),
            )),
        });
        last_phase = PhaseName::HashCheck;

        // ── Phase 3: Cleanup ────────────────────────────────────────────
        check_cancelled!();
        let _cleanup_span = debug_span!(target: "atlas_sync", "sync.full.cleanup").entered();
        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::Cleanup,
            total: 0,
        });

        let deleted_count = dirty_set.deleted.len();
        if !dirty_set.deleted.is_empty() {
            if let Err(e) = phase_cleanup_stale(&self.store, &dirty_set.deleted) {
                sink.emit(ProgressEvent::Warning {
                    phase: PhaseName::Cleanup,
                    message: format!("{e:#}"),
                });
                return Err(e);
            }
        }

        // Convert dirty paths to FileIds for per-ID cleanup before re-extraction.
        let stale_ids: Vec<_> = match dirty_set
            .dirty
            .iter()
            .map(|p| source_file_id(p))
            .collect::<Result<Vec<_>>>()
        {
            Ok(ids) => ids,
            Err(e) => {
                sink.emit(ProgressEvent::Warning {
                    phase: PhaseName::Cleanup,
                    message: format!("{e:#}"),
                });
                return Err(e);
            }
        };
        let stale_count = stale_ids.len();
        if !stale_ids.is_empty() {
            if let Err(e) = phase_cleanup_file_ids(&self.store, &stale_ids) {
                sink.emit(ProgressEvent::Warning {
                    phase: PhaseName::Cleanup,
                    message: format!("{e:#}"),
                });
                return Err(e);
            }
        }

        sink.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::Cleanup,
            succeeded: (deleted_count + stale_count) as u64,
            failed: 0,
            detail: Some(format!(
                "{deleted_count} deleted, {stale_count} stale cleaned",
            )),
        });
        last_phase = PhaseName::Cleanup;
        drop(_cleanup_span);

        let files_to_extract = dirty_set.dirty;

        // ── Phases 4-6 only when there are dirty files ──────────────────
        if !files_to_extract.is_empty() {
            // ── Phase 4: LanguageInit ───────────────────────────────────
            check_cancelled!();
            sink.emit(ProgressEvent::PhaseStarted {
                phase: PhaseName::LanguageInit,
                total: 0,
            });
            let frontend_cache = match phase_init_frontends(&files_to_extract) {
                Ok(fe) => fe,
                Err(e) => {
                    sink.emit(ProgressEvent::Warning {
                        phase: PhaseName::LanguageInit,
                        message: format!("{e:#}"),
                    });
                    return Err(e);
                }
            };
            let lang_count = frontend_cache.len();
            sink.emit(ProgressEvent::PhaseFinished {
                phase: PhaseName::LanguageInit,
                succeeded: lang_count as u64,
                failed: 0,
                detail: Some(format!("{lang_count} language frontends initialized")),
            });
            last_phase = PhaseName::LanguageInit;

            // ── Phase 5: Extraction ─────────────────────────────────────
            check_cancelled!();
            let extract_total = files_to_extract.len();
            sink.emit(ProgressEvent::PhaseStarted {
                phase: PhaseName::Extraction,
                total: extract_total as u64,
            });

            // Cancel token: shared AtomicBool so external interrupt
            // handlers (e.g. Ctrl-C) can stop the parallel extraction
            // loop early.  The phase-boundary check above already
            // handles pre-extraction cancellation.
            let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));

            // Per-file progress callback: emits ItemProgress every 50
            // files (throttled internally by phase_extract_parallel_cancellable).
            let ct = Arc::clone(&cancel_token);
            let int_cell_for_progress = std::sync::Arc::clone(&int_cell);
            let on_file_progress = move |completed: usize, _total: usize| {
                if (*int_cell_for_progress.lock().expect("cancellation check lock poisoned"))() {
                    ct.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                sink.emit(ProgressEvent::ItemProgress {
                    phase: PhaseName::Extraction,
                    completed: completed as u64,
                });
            };

            // Propagate external interrupt to cancel token before extraction
            // starts so the rayon loop can see it on the first file.
            if (*int_cell.lock().expect("cancellation check lock poisoned"))() {
                cancel_token.store(true, std::sync::atomic::Ordering::Relaxed);
            }

            let extracted = phase_extract_parallel_cancellable(
                &self.project_root,
                &files_to_extract,
                &frontend_cache,
                self.options.mode.clone(),
                None, // on_progress (once at end) — unused
                Some(&on_file_progress),
                Some(&cancel_token),
            );

            sink.emit(ProgressEvent::PhaseFinished {
                phase: PhaseName::Extraction,
                succeeded: extracted.stats.succeeded as u64,
                failed: extracted.stats.failed as u64,
                detail: Some(format!(
                    "{} succeeded, {} failed, {} symbols found",
                    extracted.stats.succeeded, extracted.stats.failed, extracted.stats.symbols,
                )),
            });
            last_phase = PhaseName::Extraction;

            stats.failed += extracted.stats.failed;
            stats.symbols = extracted.stats.symbols;

            // ── Phase 6: DbWrite ────────────────────────────────────────
            check_cancelled!();
            sink.emit(ProgressEvent::PhaseStarted {
                phase: PhaseName::DbWrite,
                total: extracted.items.len() as u64,
            });

            let write_progress = |written: u64| {
                sink.emit(ProgressEvent::ItemProgress {
                    phase: PhaseName::DbWrite,
                    completed: written,
                });
            };

            let write_stats =
                phase_write_batched(&self.store, extracted, 500, 500, write_progress, || {
                    (*int_cell.lock().expect("cancellation check lock poisoned"))()
                })?;

            stats.indexed = write_stats.written;

            sink.emit(ProgressEvent::PhaseFinished {
                phase: PhaseName::DbWrite,
                succeeded: write_stats.written as u64,
                failed: (write_stats.batch_failures + write_stats.single_failures) as u64,
                detail: Some(format!(
                    "{} written, {} batch failures, {} single failures",
                    write_stats.written, write_stats.batch_failures, write_stats.single_failures,
                )),
            });
            last_phase = PhaseName::DbWrite;
        }

        // ── Phase 7: Resolution (Structural / Full only) ────────────────
        if self.options.mode.produces_references() {
            check_cancelled!();

            // Get unresolved count for progress total
            let unresolved_total = self
                .store
                .find_unresolved_references()
                .map(|refs| refs.len() as u64)
                .unwrap_or(0);

            sink.emit(ProgressEvent::PhaseStarted {
                phase: PhaseName::Resolution,
                total: unresolved_total,
            });

            let ps = sink.progress_state();
            let graph_result = match phase_resolve_and_build(&self.store, &self.project_root, ps) {
                Ok(gr) => gr,
                Err(e) => {
                    sink.emit(ProgressEvent::Warning {
                        phase: PhaseName::Resolution,
                        message: format!("{e:#}"),
                    });
                    return Err(e);
                }
            };
            stats.resolved = graph_result.resolved;
            stats.edges_built = graph_result.edges_built;
            sink.emit(ProgressEvent::PhaseFinished {
                phase: PhaseName::Resolution,
                succeeded: graph_result.resolved as u64,
                failed: 0,
                detail: Some(format!(
                    "{} resolved, {} edges built",
                    graph_result.resolved, graph_result.edges_built,
                )),
            });
            last_phase = PhaseName::Resolution;

            // ── Phase 8: Materialize annotations ────────────────────────
            check_cancelled!();
            sink.emit(ProgressEvent::PhaseStarted {
                phase: PhaseName::AnnotationMaterialize,
                total: 0,
            });
            match phase_materialize_annotations(&self.store) {
                Ok(()) => {
                    sink.emit(ProgressEvent::PhaseFinished {
                        phase: PhaseName::AnnotationMaterialize,
                        succeeded: 1,
                        failed: 0,
                        detail: None,
                    });
                }
                Err(e) => {
                    sink.emit(ProgressEvent::Warning {
                        phase: PhaseName::AnnotationMaterialize,
                        message: format!("{e:#}"),
                    });
                    return Err(e);
                }
            }
            last_phase = PhaseName::AnnotationMaterialize;
        }

        // ── Phase 9: Build summaries (Full mode only) ───────────────────
        if self.options.mode.produces_dataflow() {
            check_cancelled!();

            // Get function count for progress total
            let all_symbols = self.store.get_all_symbols().unwrap_or_default();
            let function_count = all_symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Function)
                .count() as u64;

            sink.emit(ProgressEvent::PhaseStarted {
                phase: PhaseName::SummaryBuild,
                total: function_count,
            });

            let on_summary_progress = |completed: u64| {
                sink.emit(ProgressEvent::ItemProgress {
                    phase: PhaseName::SummaryBuild,
                    completed,
                });
            };

            match phase_build_summaries(&self.store, Some(&on_summary_progress)) {
                Ok(n) => {
                    sink.emit(ProgressEvent::PhaseFinished {
                        phase: PhaseName::SummaryBuild,
                        succeeded: n as u64,
                        failed: 0,
                        detail: Some(format!("{n} functions summarized")),
                    });
                }
                Err(e) => {
                    sink.emit(ProgressEvent::Warning {
                        phase: PhaseName::SummaryBuild,
                        message: format!("{e:#}"),
                    });
                    return Err(e);
                }
            }
            last_phase = PhaseName::SummaryBuild;
        }

        // ── Phase 10: Finalize ──────────────────────────────────────────
        check_cancelled!();
        sink.emit(ProgressEvent::PhaseStarted {
            phase: PhaseName::Finalize,
            total: 0,
        });
        if let Err(e) = phase_commit_path_alias_config(&self.store, &self.project_root) {
            sink.emit(ProgressEvent::Warning {
                phase: PhaseName::Finalize,
                message: format!("{e:#}"),
            });
            return Err(e);
        }
        if let Err(e) = phase_finalize(
            &self.store,
            &self.project_root,
            &self.options.include_patterns,
        ) {
            sink.emit(ProgressEvent::Warning {
                phase: PhaseName::Finalize,
                message: format!("{e:#}"),
            });
            return Err(e);
        }
        sink.emit(ProgressEvent::PhaseFinished {
            phase: PhaseName::Finalize,
            succeeded: 1,
            failed: 0,
            detail: None,
        });
        // Intentionally keep last_phase = SummaryBuild (or previous) so
        // Cancelled events between phases also reflect the correct
        // last-completed phase.
        // last_phase = PhaseName::Finalize; (not needed — no more phases)

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoopSink;

    /// A sink that records every event into a Vec for assertions.
    struct RecordingSink {
        events: std::sync::Mutex<Vec<ProgressEvent>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn events(&self) -> std::sync::MutexGuard<'_, Vec<ProgressEvent>> {
            self.events.lock().unwrap()
        }
    }

    impl ProgressSink for RecordingSink {
        fn emit(&self, event: ProgressEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn pipeline_emits_discovery_and_hashcheck_for_manifest_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.ts"),
            "export function greet(name: string) { return `hi ${name}`; }\n",
        )
        .unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let pipeline = IndexPipeline::new(
            Arc::clone(&store),
            dir.path().to_path_buf(),
            IndexPipelineOptions::new(ExtractionMode::Manifest),
        );

        let sink = RecordingSink::new();
        let stats = pipeline.run(&sink, &mut || false).unwrap();

        assert_eq!(stats.discovered, 1);
        assert!(stats.indexed > 0);
        assert_eq!(stats.resolved, 0);

        let events = sink.events();
        // Assert that Discovery and HashCheck PhaseStarted + PhaseFinished exist.
        let has_discovery_started = events.iter().any(|e| {
            matches!(
                e,
                ProgressEvent::PhaseStarted {
                    phase: PhaseName::Discovery,
                    ..
                }
            )
        });
        assert!(has_discovery_started, "should emit Discovery PhaseStarted");

        let has_discovery_finished = events.iter().any(|e| {
            matches!(
                e,
                ProgressEvent::PhaseFinished {
                    phase: PhaseName::Discovery,
                    ..
                }
            )
        });
        assert!(
            has_discovery_finished,
            "should emit Discovery PhaseFinished"
        );
    }

    #[test]
    fn pipeline_cancels_before_phase() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let pipeline = IndexPipeline::new(
            Arc::clone(&store),
            PathBuf::from("/nonexistent"),
            IndexPipelineOptions::new(ExtractionMode::Manifest),
        );

        let sink = RecordingSink::new();
        // Interrupt immediately
        let stats = pipeline.run(&sink, &mut || true).unwrap();

        assert_eq!(stats.discovered, 0);
        assert_eq!(stats.indexed, 0);

        let events = sink.events();
        let has_cancelled = events
            .iter()
            .any(|e| matches!(e, ProgressEvent::Cancelled { .. }));
        assert!(has_cancelled, "should emit Cancelled event");
    }

    #[test]
    fn pipeline_handles_noop_sink() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.ts"), "export const x = 1;\n").unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let pipeline = IndexPipeline::new(
            Arc::clone(&store),
            dir.path().to_path_buf(),
            IndexPipelineOptions::new(ExtractionMode::Manifest),
        );

        let stats = pipeline.run(&NoopSink, &mut || false).unwrap();

        assert!(stats.indexed > 0);
    }
}
