//! `atlas sync` — incremental sync for changed files.

use crate::commands::progress::CliProgressSink;
use crate::runtime::{CommandContext, DbMode};
use anyhow::Result;
use atlas_engine::guard_against_precision_downgrade;
use atlas_engine::progress::ProgressState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// RAII guard that sets `done` to `true` on drop, guaranteeing the spinner
/// exits even if the worker thread encounters an error.
struct DoneGuard(Arc<AtomicBool>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub fn run_with_options(project: &str, analysis: &str, force_reindex: bool) -> Result<()> {
    let mode = atlas_engine::parse_analysis_mode(analysis)?;
    let has_dataflow = mode.produces_dataflow();

    let ctx = CommandContext::open(project, DbMode::ExistingReadWrite)?;
    guard_against_precision_downgrade(&ctx.store, &mode, force_reindex, "atlas sync")?;
    let root = ctx.root.clone(); // clone before move into SyncEngine

    let engine = atlas_engine::SyncEngine::with_mode(ctx.store.clone(), root.clone(), mode);

    // ── Progress for sync ───────────────────────────────────────────────
    let progress_state = Arc::new(Mutex::new(ProgressState::new()));
    let done = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::new(AtomicBool::new(false));
    crate::tui::progress::install_ctrlc_handler(Arc::clone(&stop_flag));

    let ps = progress_state.clone();
    let done_clone = done.clone();
    let stop_w = stop_flag.clone();

    // Run sync in background thread with progress
    let handle = std::thread::spawn(move || -> Result<_> {
        let _done = DoneGuard(done_clone);
        let sink = CliProgressSink { progress: ps };
        let mut interrupted = || stop_w.load(Ordering::SeqCst);
        engine.sync(&sink, &mut interrupted)
    });

    // Start TUI (or text fallback if non-TTY), same progress contract as index.
    let was_interrupted =
        crate::tui::progress::run_progress_loop(progress_state.clone(), &done, &stop_flag);

    let worker_result = match handle.join() {
        Ok(Ok(stats)) => Ok(stats),
        Ok(Err(e)) => Err(e),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(anyhow::anyhow!("Sync worker panicked: {msg}"))
        }
    };

    if was_interrupted {
        crate::tui::progress::print_interrupted(&progress_state.lock().unwrap());
        return Err(anyhow::anyhow!("Interrupted"));
    }

    let stats = worker_result?;

    println!("\nSync complete:");
    println!("  Files reindexed: {}", stats.files_reindexed);
    println!("  Files removed:   {}", stats.files_removed);
    println!("  New symbols:     {}", stats.new_nodes);
    println!("  Resolved refs:   {}", stats.new_edges);
    if has_dataflow {
        println!(
            "  Summaries:       {} updated ({} skipped / empty)",
            stats.summaries_updated, stats.summaries_skipped
        );
    }
    println!("  Duration:        {:?}", stats.duration);

    if !stats.phase_timings.is_empty() {
        print_phase_timings(&stats.phase_timings);
    }

    Ok(())
}

fn print_phase_timings(timings: &atlas_engine::PhaseTimings) {
    println!();
    println!("Phase timings:");
    println!("  {:<20} {:>8}  Details", "Phase", "Time");
    println!("  {:-<20} {:-<8}  {:-<20}", "", "", "");

    for t in &timings.phases {
        let time_str = format_duration(t.duration_ms);
        let mut parts = Vec::new();
        if let Some(items) = t.items {
            parts.push(format!("{items} items"));
        }
        if let Some(ref note) = t.note {
            parts.push(note.clone());
        }
        println!("  {:<20} {:>8}  {}", t.phase, time_str, parts.join(", "));
    }

    println!("  {:<20} {:>8}", "Total", format_duration(timings.total_ms));
}

fn format_duration(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    } else if ms >= 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}
