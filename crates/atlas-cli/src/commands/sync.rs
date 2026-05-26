//! `atlas sync` — incremental sync for changed files.

use crate::runtime::{CommandContext, DbMode};
use anyhow::{Context, Result};
use atlas_engine::ExtractionMode;
use atlas_engine::FileLock;
use atlas_engine::progress::{ProgressPhase, ProgressState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH, Duration};

pub fn run(project: &str, analysis: &str) -> Result<()> {
    let mode = match analysis {
        "manifest" => ExtractionMode::Manifest,
        "full" => ExtractionMode::Full,
        _ => ExtractionMode::Structural,
    };

    let ctx = CommandContext::open(project, DbMode::ExistingReadWrite)?;
    let _lock = FileLock::acquire(&ctx.store)
        .context("Another atlas process is modifying this project.")?;

    let engine = atlas_engine::SyncEngine::with_mode(ctx.store.clone(), ctx.root, mode);

    // Detect and report changes
    let changed = engine.detect_changes()?;
    if changed.is_empty() {
        println!("No changes detected.");
        return Ok(());
    }

    println!("Changes detected:");
    if !changed.added.is_empty() {
        println!("  Added ({})", changed.added.len());
    }
    if !changed.modified.is_empty() {
        println!("  Modified ({})", changed.modified.len());
    }
    if !changed.deleted.is_empty() {
        println!("  Deleted ({})", changed.deleted.len());
    }

    // ── Progress for sync (simplified: text-only for fast incremental ops) ──
    let progress_state = Arc::new(Mutex::new(ProgressState::new()));
    let done = Arc::new(AtomicBool::new(false));

    let ps = progress_state.clone();
    let done_clone = done.clone();

    // Run sync in background thread with progress
    let handle = std::thread::spawn(move || -> Result<_> {
        ps.lock().unwrap().start_phase(ProgressPhase::Extraction, Some("Syncing...".into()));
        let stats = engine.sync()?;
        ps.lock().unwrap().start_phase(ProgressPhase::Finalizing, None);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();
        ctx.store.set_metadata("last_sync_time", &now)?;

        done_clone.store(true, Ordering::SeqCst);
        Ok(stats)
    });

    // Simple spinner while waiting
    let spinner = ['|', '/', '-', '\\'];
    let mut idx = 0;
    while !done.load(Ordering::SeqCst) {
        eprint!("\r  Syncing... {}", spinner[idx]);
        idx = (idx + 1) % 4;
        std::thread::sleep(Duration::from_millis(200));
    }
    eprint!("\r  Syncing... done\n");

    let stats = handle.join().unwrap()?;

    println!("\nSync complete:");
    println!("  Files reindexed: {}", stats.files_reindexed);
    println!("  Files removed:   {}", stats.files_removed);
    println!("  New symbols:     {}", stats.new_nodes);
    println!("  Resolved refs:   {}", stats.new_edges);
    println!("  Duration:        {:?}", stats.duration);

    if !stats.phase_timings.is_empty() {
        print_phase_timings(&stats.phase_timings);
    }

    Ok(())
}

fn print_phase_timings(timings: &atlas_engine::PhaseTimings) {
    println!();
    println!("Phase timings:");
    println!("  {:<20} {:>8}  {}", "Phase", "Time", "Details");
    println!("  {:-<20} {:-<8}  {:-<20}", "", "", "");

    for t in &timings.phases {
        let time_str = format_duration(t.duration_ms);
        let mut parts = Vec::new();
        if let Some(items) = t.items {
            parts.push(format!("{} items", items));
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
        format!("{}ms", ms)
    }
}
