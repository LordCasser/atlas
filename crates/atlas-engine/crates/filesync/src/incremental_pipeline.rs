//! Incremental pipeline orchestrator: structured phases for incremental sync.
//!
//! Composes the same phase functions as [`crate::index_pipeline`] but drives
//! them through [`ProgressSink`] events, interrupt checks, and scoped summary
//! rebuilding instead of full rebuilds.
//!
//! `SyncEngine::sync()` drives this pipeline so incremental sync remains
//! observable, cancellable, and consistent with the full-index pipeline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use analysis::summary::SummaryBuilder;
use anyhow::{Context, Result};
use db::Store;
use db::summary::SummaryStore;
use extraction::ExtractionMode;
use tracing;
use types::SymbolKind;

use crate::cleanup::source_file_id;
use crate::index_phases::{
    phase_build_summaries, phase_cleanup_file_ids, phase_cleanup_stale,
    phase_commit_path_alias_config, phase_extract_parallel_cancellable, phase_init_frontends,
    phase_materialize_annotations, phase_resolve_and_build, phase_write_batched,
    record_summaries_extraction_state,
};
use crate::progress::{PhaseName, ProgressEvent, ProgressSink};
use crate::sync_engine::SyncStats;

// ── IncrementalPipeline ─────────────────────────────────────────────────

/// An orchestrator for incremental sync that reuses composable phase
/// functions, emits progress through a [`ProgressSink`], and applies scoped
/// summary building.
pub struct IncrementalPipeline {
    store: Arc<Store>,
    project_root: PathBuf,
    mode: ExtractionMode,
}

impl IncrementalPipeline {
    /// Create a new incremental pipeline.
    pub fn new(store: Arc<Store>, project_root: PathBuf, mode: ExtractionMode) -> Self {
        Self {
            store,
            project_root,
            mode,
        }
    }

    /// Run the incremental sync pipeline.
    ///
    /// Each phase checks `interrupted()` before starting and emits
    /// `ProgressEvent::PhaseStarted` / `PhaseFinished` bracketing the work.
    /// Errors are forwarded as `Warning` events and then propagated.
    #[allow(clippy::too_many_lines)]
    pub fn sync(
        &self,
        sink: &dyn ProgressSink,
        interrupted: &mut dyn FnMut() -> bool,
    ) -> Result<SyncStats> {
        // ── Phase 1: ChangeDetection ───────────────────────────────
        let phase = PhaseName::Custom("ChangeDetection");
        if interrupted() {
            sink.emit(ProgressEvent::Cancelled { last_phase: phase });
            return Ok(SyncStats::default());
        }
        sink.emit(ProgressEvent::PhaseStarted { phase, total: 0 });

        let changed = crate::detector::detect_changes(&self.project_root, &self.store)
            .context("Failed to detect changes against indexed content hashes")?;

        sink.emit(ProgressEvent::PhaseFinished {
            phase,
            succeeded: changed.total() as u64,
            failed: 0,
            detail: Some(format!(
                "{} added, {} modified, {} deleted",
                changed.added.len(),
                changed.modified.len(),
                changed.deleted.len()
            )),
        });

        // ── Phase 2: AliasCheck ────────────────────────────────────
        let phase = PhaseName::Custom("AliasCheck");
        if interrupted() {
            sink.emit(ProgressEvent::Cancelled { last_phase: phase });
            return Ok(SyncStats {
                files_changed: changed.total(),
                ..Default::default()
            });
        }
        sink.emit(ProgressEvent::PhaseStarted { phase, total: 0 });

        let alias_changed =
            resolution::PathAliasConfig::has_changed(&self.store, &self.project_root).map_err(
                |e| {
                    sink.emit(ProgressEvent::Warning {
                        phase,
                        message: format!("{e:#}"),
                    });
                    e
                },
            )?;

        if alias_changed {
            sink.emit(ProgressEvent::Warning {
                phase,
                message: "Path alias config changed; edges will be fully invalidated".into(),
            });
        }

        sink.emit(ProgressEvent::PhaseFinished {
            phase,
            succeeded: 1,
            failed: 0,
            detail: None,
        });

        // Short-circuit if nothing to do
        if changed.is_empty() && !alias_changed {
            return Ok(SyncStats {
                files_changed: 0,
                ..Default::default()
            });
        }

        // ── Convert absolute paths to project-relative ──────────────
        let deleted_rel = to_relative_paths(&changed.deleted, &self.project_root);
        let modified_rel = to_relative_paths(&changed.modified, &self.project_root);
        let added_rel = to_relative_paths(&changed.added, &self.project_root);

        // ── Phase 3: Cleanup ───────────────────────────────────────
        let phase = PhaseName::Cleanup;
        if interrupted() {
            sink.emit(ProgressEvent::Cancelled { last_phase: phase });
            return Ok(SyncStats {
                files_changed: changed.total(),
                ..Default::default()
            });
        }
        sink.emit(ProgressEvent::PhaseStarted {
            phase,
            total: (deleted_rel.len() + modified_rel.len()) as u64,
        });

        if !deleted_rel.is_empty() {
            phase_cleanup_stale(&self.store, &deleted_rel).map_err(|e| {
                sink.emit(ProgressEvent::Warning {
                    phase,
                    message: format!("Cleanup of deleted files failed: {e:#}"),
                });
                e
            })?;
        }

        if !modified_rel.is_empty() {
            let modified_file_ids = modified_rel
                .iter()
                .map(|p| source_file_id(p))
                .collect::<Result<Vec<_>>>()
                .context("Failed to derive source identity for modified-file cleanup")?;
            if !modified_file_ids.is_empty() {
                phase_cleanup_file_ids(&self.store, &modified_file_ids).map_err(|e| {
                    sink.emit(ProgressEvent::Warning {
                        phase,
                        message: format!("Cleanup of modified files failed: {e:#}"),
                    });
                    e
                })?;
            }
        }

        // Alias config invalidation (independent of file changes)
        if alias_changed {
            self.store
                .invalidate_all_references()
                .context("Failed to invalidate references for alias config change")
                .map_err(|e| {
                    sink.emit(ProgressEvent::Warning {
                        phase,
                        message: format!("{e:#}"),
                    });
                    e
                })?;
            self.store
                .delete_all_edges()
                .context("Failed to delete edges for alias config change")
                .map_err(|e| {
                    sink.emit(ProgressEvent::Warning {
                        phase,
                        message: format!("{e:#}"),
                    });
                    e
                })?;
        }

        let cleanup_total = (deleted_rel.len() + modified_rel.len()) as u64;
        sink.emit(ProgressEvent::PhaseFinished {
            phase,
            succeeded: cleanup_total,
            failed: 0,
            detail: Some(format!(
                "{} cleaned, {} edges invalidated",
                cleanup_total,
                if alias_changed { "all" } else { "none" }
            )),
        });

        // Files to (re-)extract: modified + added
        let to_extract_rel: Vec<PathBuf> = modified_rel
            .iter()
            .chain(added_rel.iter())
            .cloned()
            .collect();

        let mut stats = SyncStats {
            files_changed: changed.total(),
            files_removed: changed.deleted.len(),
            ..Default::default()
        };

        if !to_extract_rel.is_empty() {
            // ── Phase 4: LanguageInit ─────────────────────────────
            let phase = PhaseName::LanguageInit;
            if interrupted() {
                sink.emit(ProgressEvent::Cancelled { last_phase: phase });
                return Ok(stats);
            }
            sink.emit(ProgressEvent::PhaseStarted {
                phase,
                total: to_extract_rel.len() as u64,
            });

            let frontends = phase_init_frontends(&to_extract_rel).map_err(|e| {
                sink.emit(ProgressEvent::Warning {
                    phase,
                    message: format!("{e:#}"),
                });
                e
            })?;

            sink.emit(ProgressEvent::PhaseFinished {
                phase,
                succeeded: frontends.len() as u64,
                failed: 0,
                detail: None,
            });

            // ── Phase 5: Extraction ──────────────────────────────
            let phase = PhaseName::Extraction;
            if interrupted() {
                sink.emit(ProgressEvent::Cancelled { last_phase: phase });
                return Ok(stats);
            }
            sink.emit(ProgressEvent::PhaseStarted {
                phase,
                total: to_extract_rel.len() as u64,
            });

            // Cancel token for mid-extraction interrupt (see IndexPipeline
            // for the same pattern).
            let cancel_token = std::sync::atomic::AtomicBool::new(false);

            let on_file_progress = |completed: usize, _total: usize| {
                sink.emit(ProgressEvent::ItemProgress {
                    phase,
                    completed: completed as u64,
                });
            };

            let extracted = phase_extract_parallel_cancellable(
                &self.project_root,
                &to_extract_rel,
                &frontends,
                self.mode.clone(),
                Some(&on_file_progress),
                Some(&cancel_token),
            );

            sink.emit(ProgressEvent::PhaseFinished {
                phase,
                succeeded: extracted.stats.succeeded as u64,
                failed: extracted.stats.failed as u64,
                detail: Some(format!(
                    "{} succeeded, {} failed, {} symbols",
                    extracted.stats.succeeded, extracted.stats.failed, extracted.stats.symbols
                )),
            });

            stats.new_nodes = extracted.stats.symbols;

            // ── Phase 6: DbWrite ─────────────────────────────────
            let phase = PhaseName::DbWrite;
            if interrupted() {
                sink.emit(ProgressEvent::Cancelled { last_phase: phase });
                return Ok(stats);
            }
            sink.emit(ProgressEvent::PhaseStarted {
                phase,
                total: extracted.items.len() as u64,
            });

            let write_stats = phase_write_batched(
                &self.store,
                extracted,
                500,
                500,
                |written| {
                    if written % 50 == 0 {
                        sink.emit(ProgressEvent::ItemProgress {
                            phase,
                            completed: written,
                        });
                    }
                },
                &mut *interrupted,
            )
            .map_err(|e| {
                sink.emit(ProgressEvent::Warning {
                    phase,
                    message: format!("{e:#}"),
                });
                e
            })?;

            stats.files_reindexed = write_stats.written;

            sink.emit(ProgressEvent::PhaseFinished {
                phase,
                succeeded: write_stats.written as u64,
                failed: (write_stats.batch_failures + write_stats.single_failures) as u64,
                detail: Some(format!(
                    "{} written ({} batch failures, {} single failures)",
                    write_stats.written, write_stats.batch_failures, write_stats.single_failures
                )),
            });
        }

        // ── Phase 7: Resolution ───────────────────────────────────
        if self.mode.produces_references() {
            let phase = PhaseName::Resolution;
            if interrupted() {
                sink.emit(ProgressEvent::Cancelled { last_phase: phase });
                return Ok(stats);
            }

            let t_count = Instant::now();
            let unresolved_total = self
                .store
                .count_unresolved_references()
                .inspect(|&count| {
                    let elapsed_ms = t_count.elapsed().as_millis() as u64;
                    tracing::info!(
                        target: "atlas_sync",
                        progress_total_load_ms = elapsed_ms,
                        unresolved_refs = count,
                        "sync.progress_total_load"
                    );
                })
                .context("Failed to count unresolved references")?;
            sink.emit(ProgressEvent::PhaseStarted {
                phase,
                total: unresolved_total,
            });

            let ps = sink.progress_state();
            let graph_result = phase_resolve_and_build(&self.store, &self.project_root, ps)
                .map_err(|e| {
                    sink.emit(ProgressEvent::Warning {
                        phase,
                        message: format!("{e:#}"),
                    });
                    e
                })?;

            stats.new_edges = graph_result.edges_written;
            if graph_result.edges_written < graph_result.edges_built {
                return Err(anyhow::anyhow!(
                    "edge persistence failed: {} built, {} written — structural index is incomplete",
                    graph_result.edges_built,
                    graph_result.edges_written,
                ));
            }

            sink.emit(ProgressEvent::PhaseFinished {
                phase,
                succeeded: graph_result.resolved as u64,
                failed: 0,
                detail: Some(format!(
                    "{} resolved, {} edges built ({} written)",
                    graph_result.resolved, graph_result.edges_built, graph_result.edges_written,
                )),
            });
        }

        // ── Phase 8: Annotations ──────────────────────────────────
        if self.mode.produces_references() {
            let phase = PhaseName::AnnotationMaterialize;
            if interrupted() {
                sink.emit(ProgressEvent::Cancelled { last_phase: phase });
                return Ok(stats);
            }
            sink.emit(ProgressEvent::PhaseStarted { phase, total: 0 });

            phase_materialize_annotations(&self.store).map_err(|e| {
                sink.emit(ProgressEvent::Warning {
                    phase,
                    message: format!("Failed to materialize annotations: {e:#}"),
                });
                e
            })?;

            sink.emit(ProgressEvent::PhaseFinished {
                phase,
                succeeded: 1,
                failed: 0,
                detail: None,
            });
        }

        // ── Phase 9: SummaryBuild ─────────────────────────────────
        if self.mode.produces_dataflow() {
            let phase = PhaseName::SummaryBuild;
            if interrupted() {
                sink.emit(ProgressEvent::Cancelled { last_phase: phase });
                return Ok(stats);
            }

            let total_indexed = self
                .store
                .count_files()
                .context("Failed to count indexed files for summary planning")?;
            let changed_count = changed.modified.len() + changed.added.len();

            if total_indexed == 0 || (changed_count as f64) >= 0.3 * (total_indexed as f64) {
                // ≥30% changed — full rebuild is more efficient
                sink.emit(ProgressEvent::Warning {
                    phase,
                    message: format!(
                        "{changed_count}/{total_indexed} files changed (≥30%), rebuilding all summaries",
                    ),
                });

                // Get function count for progress total
                let all_symbols = self
                    .store
                    .get_all_symbols()
                    .context("Failed to load functions for summary planning")?;
                let function_count = all_symbols
                    .iter()
                    .filter(|s| s.kind == SymbolKind::Function)
                    .count() as u64;
                sink.emit(ProgressEvent::PhaseStarted {
                    phase,
                    total: function_count,
                });

                let on_progress = |completed: u64| {
                    sink.emit(ProgressEvent::ItemProgress { phase, completed });
                };

                let full_summaries = phase_build_summaries(&self.store, Some(&on_progress))
                    .map_err(|e| {
                        sink.emit(ProgressEvent::Warning {
                            phase,
                            message: format!("Failed to build summaries: {e:#}"),
                        });
                        e
                    })?;
                stats.summaries_updated = full_summaries;
                stats.summaries_skipped = 0;

                sink.emit(ProgressEvent::PhaseFinished {
                    phase,
                    succeeded: full_summaries as u64,
                    failed: 0,
                    detail: Some(format!("{full_summaries} summaries rebuilt (full)")),
                });
            } else {
                // Scoped: rebuild summaries only for changed functions.
                sink.emit(ProgressEvent::PhaseStarted { phase, total: 0 });
                let changed_rel: Vec<PathBuf> = modified_rel
                    .iter()
                    .chain(added_rel.iter())
                    .cloned()
                    .collect();

                let changed_file_ids: Vec<types::FileId> = changed_rel
                    .iter()
                    .map(|p| source_file_id(p))
                    .collect::<Result<Vec<_>>>()
                    .context("Failed to derive source identity for summary rebuild")?;

                let mut updated = 0usize;
                let mut skipped = 0usize;

                if !changed_file_ids.is_empty() {
                    let symbols = self
                        .store
                        .find_symbols_by_files(&changed_file_ids)
                        .context("Failed to load changed functions for summary rebuild")?;
                    let function_symbols: Vec<_> = symbols
                        .iter()
                        .filter(|s| s.kind == SymbolKind::Function)
                        .collect();

                    for sym in &function_symbols {
                        let summary =
                            SummaryStore::build_for_function(&self.store, &sym.id, |s, fid| {
                                SummaryBuilder::build(s, fid, None)
                            })
                            .map_err(|error| {
                                sink.emit(ProgressEvent::Warning {
                                    phase,
                                    message: format!(
                                        "Failed to build summary for {}: {error:#}",
                                        sym.qualified_name
                                    ),
                                });
                                error
                            })?;
                        if summary.is_empty() {
                            skipped += 1;
                        } else {
                            updated += 1;
                        }
                    }
                }

                record_summaries_extraction_state(&self.store).map_err(|e| {
                    sink.emit(ProgressEvent::Warning {
                        phase,
                        message: format!("Failed to record summary capability state: {e:#}"),
                    });
                    e
                })?;

                stats.summaries_updated = updated;
                stats.summaries_skipped = skipped;

                sink.emit(ProgressEvent::PhaseFinished {
                    phase,
                    succeeded: updated as u64,
                    failed: 0,
                    detail: Some(format!("{updated} updated, {skipped} empty")),
                });
            }
        }

        // ── Phase 10: ConfigCommit ────────────────────────────────
        let phase = PhaseName::Custom("ConfigCommit");
        if interrupted() {
            sink.emit(ProgressEvent::Cancelled { last_phase: phase });
            return Ok(stats);
        }
        sink.emit(ProgressEvent::PhaseStarted { phase, total: 0 });

        phase_commit_path_alias_config(&self.store, &self.project_root).map_err(|e| {
            sink.emit(ProgressEvent::Warning {
                phase,
                message: format!("{e:#}"),
            });
            e
        })?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();
        self.store
            .set_metadata("last_sync_time", &now)
            .map_err(|e| {
                sink.emit(ProgressEvent::Warning {
                    phase,
                    message: format!("Failed to commit sync metadata: {e:#}"),
                });
                e
            })?;

        sink.emit(ProgressEvent::PhaseFinished {
            phase,
            succeeded: 1,
            failed: 0,
            detail: None,
        });

        Ok(stats)
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Convert absolute paths to project-relative paths.
fn to_relative_paths(paths: &[PathBuf], root: &Path) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| p.strip_prefix(root).unwrap_or(p).to_path_buf())
        .collect()
}
