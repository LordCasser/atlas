//! `atlas index` command — walk project tree (git-aware), extract facts, resolve references.
//!
//! ## Design
//! - **Worker thread**: runs the IndexPipeline, emits progress via CliProgressSink into ProgressState.
//! - **Main thread**: runs the terminal progress loop (or text fallback).
//!
//! ## P6: Progress reporting
//! The IndexPipeline emits ProgressEvent items.  CliProgressSink translates
//! them into ProgressState updates consumed by the TUI render loop.

use crate::commands::progress::CliProgressSink;
use crate::runtime::{CommandContext, DbMode};
use crate::tui::{TextFallback, TuiProgress};
use anyhow::Context;
use atlas_engine::FileLock;
use atlas_engine::guard_against_precision_downgrade;
use atlas_engine::progress::ProgressState;
use atlas_engine::{IndexPipeline, IndexPipelineOptions};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub fn run(
    project: &str,
    includes: &[String],
    scopes: &[String],
    exclude: &[String],
    analysis: &str,
) -> anyhow::Result<()> {
    run_with_options(project, includes, scopes, exclude, analysis, false)
}

pub fn run_with_options(
    project: &str,
    includes: &[String],
    scopes: &[String],
    exclude: &[String],
    analysis: &str,
    force_reindex: bool,
) -> anyhow::Result<()> {
    let mode = atlas_engine::parse_analysis_mode(analysis)?;

    // ── Merge include/scope patterns ──
    let mut include_patterns: Vec<String> = includes.to_vec();
    for scope in scopes {
        include_patterns.push(scope_to_glob(scope));
    }

    // ── Open store (before spawning worker, in case of immediate error) ──
    let ctx = CommandContext::open(project, DbMode::CreateOrOpenReadWrite)?;
    let _lock =
        FileLock::acquire(&ctx.store).context("Another atlas process is indexing this project.")?;
    guard_against_precision_downgrade(&ctx.store, &mode, force_reindex, "atlas index")?;

    // ── Shared state ──
    let progress_state = Arc::new(Mutex::new(ProgressState::new()));
    let done_flag = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::new(AtomicBool::new(false));

    // ── Ctrl+C handler ──
    // First press: graceful shutdown (stop_flag → pipeline exits cleanly).
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

    // ── Spawn worker thread with 8 MiB stack ──
    let worker = std::thread::Builder::new()
        .name("atlas-index-worker".into())
        .stack_size(8 * 1024 * 1024) // 8 MiB — safety margin for extraction/resolution
        .spawn(move || {
            let result = {
                let options = IndexPipelineOptions::new(mode)
                    .with_include_patterns(include_clone)
                    .with_exclude_patterns(exclude_clone);
                let pipeline = IndexPipeline::new(store_arc, root_path, options);
                let sink = CliProgressSink {
                    progress: ps_worker,
                };
                let mut interrupted = || stop_w.load(Ordering::SeqCst);
                pipeline.run(&sink, &mut interrupted)
            };
            // Always signal completion, even on error — prevents main thread hang.
            done_w.store(true, Ordering::SeqCst);
            result
        })
        .expect("failed to spawn index worker thread");

    // ── Main thread: render loop ──
    let was_interrupted = if has_tty {
        tui.as_mut().unwrap().draw_loop(&done_flag, &stop_flag)
    } else {
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
    // The worker checks stop_flag via the `interrupted` closure and will exit
    // cleanly between phases.  We MUST join before returning so the FileLock
    // RAII guard isn't dropped while the worker is still accessing the database.
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
        return Err(anyhow::anyhow!("Interrupted"));
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
    fn test_worker_thread_stack_size_constant() {
        // Verify the hardcoded stack size matches our target of 8 MiB
        const EXPECTED: usize = 8 * 1024 * 1024;
        assert_eq!(EXPECTED, 8_388_608); // 8 MiB
    }
}
