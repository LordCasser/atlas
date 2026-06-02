//! `atlas index` command — walk project tree (git-aware), extract facts, resolve references.
//!
//! ## Design
//! - **Worker thread**: runs the 9-phase indexing pipeline, updates ProgressState.
//! - **Main thread**: runs the terminal progress loop (or text fallback).
//! - **Phase 1 (parallel)**: Extract all files using Rayon — CPU-bound, no SQLite access.
//! - **Phase 2 (sequential)**: Insert extracted facts into the store — SQLite single-writer.
//! - **Phase 3**: Resolve all references (parallel matching + serial write).
//!
//! ## P6: Progress reporting
//! Every phase reports progress via `ProgressState`:
//! - Parallel phases (Extraction, Resolution Phase 1, EdgeBuilding): AtomicU64 counters.
//! - Serial phases (HashCheck, DB Write, Resolution Phase 2): direct `set_current()`.

use crate::runtime::{CommandContext, DbMode};
use crate::tui::{TextFallback, TuiProgress};
use anyhow::Context;
use atlas_engine::ExtractionMode;
use atlas_engine::FileLock;
use atlas_engine::Language;
use atlas_engine::LanguageFrontend;
use atlas_engine::PerLanguageStats;
use atlas_engine::progress::{ProgressPhase, ProgressState};
use atlas_engine::{self, LanguageRegistry, ParseWorkerPool, WorkerConfig};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

pub fn run(
    project: &str,
    includes: &[String],
    scopes: &[String],
    exclude: &[String],
    analysis: &str,
) -> anyhow::Result<()> {
    let mode = match analysis {
        "manifest" => ExtractionMode::Manifest,
        "full" => ExtractionMode::Full,
        _ => ExtractionMode::Structural,
    };

    // ── Configure rayon thread pool (once, idempotent) ──────────────────
    // macOS spawns threads with a 512 KB stack by default, which overflows
    // during tree-sitter's recursive-descent parsing of deeply-nested files
    // (e.g. Linux kernel headers).  4 MB matches the tree-sitter playground
    // convention and is safe on all platforms.
    //
    // `build_global()` can only succeed once per process; subsequent calls
    // (e.g. in integration tests that run `index::run()` multiple times) will
    // error.  Using `Once` ensures we only attempt the first time.
    static RAYON_INIT: std::sync::Once = std::sync::Once::new();
    RAYON_INIT.call_once(|| {
        rayon::ThreadPoolBuilder::new()
            .stack_size(4 * 1024 * 1024)
            .build_global()
            .expect("failed to initialise rayon thread pool");
    });

    // ── Merge include/scope patterns ──
    let mut include_patterns: Vec<String> = includes.to_vec();
    for scope in scopes {
        include_patterns.push(scope_to_glob(scope));
    }

    // ── Open store (before spawning worker, in case of immediate error) ──
    let ctx = CommandContext::open(project, DbMode::CreateOrOpenReadWrite)?;
    let _lock =
        FileLock::acquire(&ctx.store).context("Another atlas process is indexing this project.")?;

    // ── Shared state ──
    let progress_state = Arc::new(Mutex::new(ProgressState::new()));
    let done_flag = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::new(AtomicBool::new(false));

    // ── Ctrl+C handler ──
    // First press: graceful shutdown (stop_flag → main thread exits draw loop).
    // Second press: immediate exit (terminal may be stuck; OS-level kill).
    let stop = stop_flag.clone();
    let press_count = Arc::new(AtomicU64::new(0));
    let pc = press_count.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        let n = pc.fetch_add(1, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        if n >= 1 {
            // Second press — exit immediately.
            std::process::exit(1);
        }
    }) {
        eprintln!("warning: could not install Ctrl+C handler: {e}");
    }

    // ── Start TUI (or text fallback if non-TTY) ──
    let mut tui = TuiProgress::try_init(progress_state.clone());
    let has_tty = tui.is_some();

    // ── Clone state for worker ──
    let ps_worker = progress_state.clone();
    let done_w = done_flag.clone();
    let stop_w = stop_flag.clone();
    let root_path = ctx.root.as_path().to_path_buf();
    let store_arc = ctx.store.clone();
    let store_for_main = store_arc.clone();
    let include_clone = include_patterns.clone();
    let exclude_clone = exclude.to_vec();

    // ── Spawn worker thread ──
    let worker = std::thread::spawn(move || -> anyhow::Result<()> {
        let result = (|| -> anyhow::Result<()> {
            let root = root_path;
            let store = store_arc;
            let include_patterns = include_clone;
            let exclude = exclude_clone;
            let ps = ps_worker;

            // Helper: check stop flag
            let interrupted = || stop_w.load(Ordering::SeqCst);

            // ── Discovery ──
            ps.lock()
                .unwrap()
                .start_phase(ProgressPhase::Discovery, None);
            let discovered = atlas_engine::phase_discover(&root, &include_patterns, &exclude)
                .context("Failed to discover files")?;
            if discovered.is_empty() {
                ps.lock()
                    .unwrap()
                    .start_phase(ProgressPhase::Finalizing, None);
                anyhow::bail!("No recognizable source files found in {}", root.display());
            }
            let total = discovered.len();
            ps.lock()
                .unwrap()
                .start_phase(ProgressPhase::HashCheck, Some(format!("{total} files")));

            if interrupted() {
                return Ok(());
            }

            // ── Hash check ──
            let hash_result = atlas_engine::phase_dirty_check(&store, &discovered, &root)?;
            let dirty = &hash_result.dirty;
            let reused = hash_result.clean_count;
            ps.lock().unwrap().start_phase(
                ProgressPhase::Cleanup,
                Some(format!("{} dirty / {} reused", dirty.len(), reused)),
            );

            if interrupted() {
                return Ok(());
            }

            // ── Delete stale data ──
            if !hash_result.deleted.is_empty() {
                let deleted_count = hash_result.deleted.len() as u64;
                ps.lock().unwrap().set_total(deleted_count);
                atlas_engine::phase_cleanup_stale(&store, &hash_result.deleted)?;
                ps.lock().unwrap().set_current(deleted_count);
            }

            let languages: Vec<Language> = dirty
                .iter()
                .filter_map(|p| Language::from_path(p))
                .fold(Vec::new(), |mut acc, lang| {
                    if !acc.contains(&lang) {
                        acc.push(lang);
                    }
                    acc
                });

            if interrupted() {
                return Ok(());
            }

            // ── Language init ──
            ps.lock().unwrap().start_phase(
                ProgressPhase::LanguageInit,
                Some(format!("{} languages", languages.len())),
            );
            let _registry = LanguageRegistry::new(&languages).or_else(|e| {
                let available: Vec<Language> = languages
                    .iter()
                    .filter(|l| LanguageRegistry::new(&[**l]).is_ok())
                    .copied()
                    .collect();
                if available.is_empty() {
                    Err(e)
                } else {
                    LanguageRegistry::new(&available)
                }
            })?;
            let frontend_cache: HashMap<Language, LanguageFrontend> = languages
                .iter()
                .filter_map(|&lang| atlas_engine::create_frontend(lang).map(|fe| (lang, fe)))
                .collect();

            if interrupted() {
                return Ok(());
            }

            // ── Extraction (parallel) ──
            let dirty_total = dirty.len();
            ps.lock().unwrap().start_phase(
                ProgressPhase::Extraction,
                Some(format!("{dirty_total} files")),
            );
            ps.lock().unwrap().set_total(dirty_total as u64);

            let pool = ParseWorkerPool::new(WorkerConfig::default());
            let extracted_count = AtomicUsize::new(0);
            let per_lang_mutex = Mutex::new(PerLanguageStats::new());
            let fc = &frontend_cache;
            let extract_counter = ps.lock().unwrap().atomic_current.clone();
            let count_atomic = &extracted_count;

            let results: Vec<_> = dirty
                .par_iter()
                .filter_map(|rel_path| {
                    if interrupted() {
                        return None;
                    }
                    let abs_path = root.join(rel_path);
                    let lang = Language::from_path(rel_path)?;
                    let frontend = fc.get(&lang)?;
                    let file_start = Instant::now();
                    let result = crate::runtime::extract_one(
                        &pool,
                        &abs_path,
                        &root,
                        lang,
                        frontend,
                        mode.clone(),
                    );
                    let extract_ms = file_start.elapsed().as_millis() as u64;

                    let _count = count_atomic.fetch_add(1, Ordering::Relaxed);
                    extract_counter.fetch_add(1, Ordering::Relaxed);

                    let (facts_opt, failed, fail_cat) = match result {
                        Ok(facts) => (Some(facts), false, None),
                        Err(ref _e) => (None, true, Some("extraction_error")),
                    };
                    {
                        per_lang_mutex
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .record_file(lang, extract_ms, failed, fail_cat);
                    }
                    facts_opt.map(|facts| atlas_engine::ExtractedFile {
                        rel_path: rel_path.clone(),
                        language: lang,
                        facts,
                    })
                })
                .collect();

            if interrupted() {
                return Ok(());
            }

            let extracted = results;
            let extracted_count = extracted.len();
            let _failed_count = dirty_total.saturating_sub(extracted_count);

            // ── Clean stale facts ──
            ps.lock().unwrap().start_phase(
                ProgressPhase::Cleanup,
                Some(format!("{extracted_count} re-indexed")),
            );
            let file_ids: Vec<_> = extracted.iter().map(|ef| ef.facts.file.file_id).collect();
            atlas_engine::phase_cleanup_file_ids(&store, &file_ids)
                .context("Failed to clean stale facts")?;

            if interrupted() {
                return Ok(());
            }

            // ── DB Write (serial, with progress) ──
            ps.lock().unwrap().start_phase(
                ProgressPhase::DbWrite,
                Some(format!("{extracted_count} files")),
            );
            ps.lock().unwrap().set_total(extracted_count as u64);

            let extracted_files = atlas_engine::ExtractedFiles {
                items: extracted,
                stats: atlas_engine::ExtractionPhaseStats {
                    attempted: dirty_total,
                    succeeded: extracted_count,
                    failed: dirty_total.saturating_sub(extracted_count),
                    symbols: 0,
                },
            };

            let _write_stats = atlas_engine::phase_write_batched(
                &store,
                &extracted_files,
                500,
                500,
                |written| {
                    ps.lock().unwrap().set_current(written);
                },
                &interrupted,
            )?;

            if interrupted() {
                return Ok(());
            }

            // ── Manifest-only early return ──
            // Manifest mode only extracts symbols; skip resolution, edge building,
            // annotation materialization, and summary building.
            if matches!(mode, ExtractionMode::Manifest) {
                // Still commit path alias config and finalize metadata.
                atlas_engine::phase_commit_path_alias_config(&store, &root)?;
                atlas_engine::phase_finalize(&store, &root, &include_patterns)?;
                // Signal progress complete so TUI doesn't wait indefinitely.
                ps.lock().unwrap().start_phase(
                    ProgressPhase::Finalizing,
                    Some("manifest index complete".into()),
                );
                return Ok(());
            }

            // ── Resolution (parallel matching + serial write) ──
            let unresolved = store.get_stats()?.unresolved_references;
            ps.lock().unwrap().start_phase(
                ProgressPhase::Resolution,
                Some(format!("{unresolved} references")),
            );
            ps.lock().unwrap().set_total(unresolved as u64);

            let path_alias = atlas_engine::PathAliasConfig::resolver(&root);

            let path_alias_config_changed =
                atlas_engine::PathAliasConfig::has_changed(&store, &root)?;
            if path_alias_config_changed {
                store.invalidate_all_references()?;
                store.delete_all_edges()?;
            }

            let mut resolver =
                atlas_engine::ReferenceResolver::with_path_alias(store.clone(), path_alias);
            let (resolved, _stats) =
                resolver.resolve_all_parallel(store.clone(), Some(&ps), None)?;

            if interrupted() {
                return Ok(());
            }

            // ── Edge building (already par_iter internally) ──
            ps.lock()
                .unwrap()
                .start_phase(ProgressPhase::EdgeBuilding, None);
            let builder = atlas_engine::GraphBuilder::new(store.clone());
            let _build_stats = builder.build_all(&resolved);
            ps.lock().unwrap().set_current(resolved.len() as u64);

            // ── Materialize user annotations as edges ──
            if let Err(e) = atlas_engine::phase_materialize_annotations(&store) {
                eprintln!("Warning: failed to materialize annotations: {e}");
            }

            // ── Summary build (Schema v3: persist function summaries) ──
            ps.lock().unwrap().start_phase(
                ProgressPhase::Finalizing,
                Some("Building summaries...".into()),
            );
            let _summary_stats = atlas_engine::phase_build_summaries(&store)?;
            if interrupted() {
                return Ok(());
            }

            // ── Finalize ──
            ps.lock()
                .unwrap()
                .start_phase(ProgressPhase::Finalizing, None);
            atlas_engine::phase_commit_path_alias_config(&store, &root)?;
            atlas_engine::phase_finalize(&store, &root, &include_patterns)?;
            Ok(())
        })();
        // Always signal completion, even on error — prevents main thread hang
        done_w.store(true, Ordering::SeqCst);
        result
    });

    // ── Main thread: render loop ──
    let was_interrupted = if has_tty {
        tui.as_mut().unwrap().draw_loop(&done_flag, &stop_flag)
    } else {
        // Text fallback loop
        let mut fb = TextFallback::new(progress_state.clone());
        loop {
            fb.tick();
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            if done_flag.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        stop_flag.load(Ordering::SeqCst) // was_interrupted = stop_flag is set
    };

    // ── Join worker unconditionally ──
    // The worker checks stop_flag at phase boundaries and will exit cleanly.
    // We MUST join before returning so the FileLock RAII guard (line 89)
    // isn't dropped while the worker is still accessing the database.
    if was_interrupted {
        tracing::info!("interrupted: waiting for worker to finish...");
    }
    let worker_result = worker
        .join()
        .unwrap_or_else(|e| Err(anyhow::anyhow!("Worker thread panicked: {e:?}")));

    // Report worker result (whether it completed or was interrupted).
    if let Err(ref e) = worker_result {
        if was_interrupted {
            tracing::warn!("worker thread returned error during interrupt: {:?}", e);
        }
    }

    if was_interrupted {
        if has_tty {
            tui.take()
                .unwrap()
                .interrupt(&progress_state.lock().unwrap());
        } else {
            eprintln!();
            crate::tui::progress::print_interrupted(&progress_state.lock().unwrap());
        }
        return Ok(());
    }

    // Normal completion: propagate worker errors.
    worker_result?;

    let db_stats = store_for_main.get_stats()?;

    if has_tty {
        // Restore the inline TUI, then print the final summary as normal
        // command output so the shell prompt follows it immediately.
        tui.take().unwrap().finish(
            db_stats.total_files as u64,
            db_stats.total_symbols as u64,
            db_stats.total_edges as u64,
        );
    } else {
        // Text fallback (non-TTY): print summary to stdout.
        println!("Database status:");
        println!("  Files:    {}", db_stats.total_files);
        println!("  Symbols:  {}", db_stats.total_symbols);
        println!("  Edges:    {}", db_stats.total_edges);
        println!("\nIndex complete.");
    }

    Ok(())
}

// ── Scope helpers ──────────────────────────────────────────────────────────

fn scope_to_glob(scope: &str) -> String {
    if scope.contains('*') {
        scope.to_string()
    } else {
        format!("{}/**", scope.trim_end_matches('/'))
    }
}

#[allow(dead_code)]
fn indexed_scope_json(patterns: &[String]) -> String {
    if patterns.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(patterns).unwrap_or_else(|_| "[]".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_to_glob_bare_dir() {
        assert_eq!(scope_to_glob("drivers/net"), "drivers/net/**");
    }
    #[test]
    fn scope_to_glob_trailing_slash() {
        assert_eq!(scope_to_glob("net/"), "net/**");
    }
    #[test]
    fn scope_to_glob_already_glob() {
        assert_eq!(scope_to_glob("src/**/*.rs"), "src/**/*.rs");
    }
    #[test]
    fn indexed_scope_json_empty() {
        assert_eq!(indexed_scope_json(&[]), "[]");
    }
}
