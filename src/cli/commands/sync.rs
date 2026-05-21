//! `atlas sync` — incremental sync for changed files.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;

pub fn run(project: &str) -> Result<()> {
    let root = Path::new(project)
        .canonicalize()
        .with_context(|| format!("Project directory not found: {}", project))?;

    let atlas_dir = root.join(".atlas");
    if !atlas_dir.is_dir() {
        anyhow::bail!(
            "No .atlas directory found in '{}'. Run `atlas init` first.",
            root.display()
        );
    }

    let store = Arc::new(crate::db::Store::open(&root).context("Failed to open .atlas database")?);

    let engine = crate::sync::SyncEngine::new(store.clone(), root.clone());

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

    // Run sync
    let stats = engine.sync()?;

    println!("\nSync complete:");
    println!("  Files reindexed: {}", stats.files_reindexed);
    println!("  Files removed:   {}", stats.files_removed);
    println!("  New symbols:     {}", stats.new_nodes);
    println!("  Resolved refs:   {}", stats.new_edges);
    println!("  Duration:        {:?}", stats.duration);

    print_phase_timings(&stats.phase_timings);

    Ok(())
}

/// Print phase timing breakdown in aligned columns.
fn print_phase_timings(timings: &crate::types::PhaseTimings) {
    if timings.is_empty() {
        return;
    }
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
