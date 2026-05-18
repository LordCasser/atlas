//! `atlas sync` — incremental sync for changed files.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;

pub fn run(project: &str) -> Result<()> {
    let root = Path::new(project).canonicalize().with_context(|| {
        format!("Project directory not found: {}", project)
    })?;

    let atlas_dir = root.join(".atlas");
    if !atlas_dir.is_dir() {
        anyhow::bail!(
            "No .atlas directory found in '{}'. Run `atlas init` first.",
            root.display()
        );
    }

    let store = Arc::new(
        crate::db::Store::open(&root)
            .context("Failed to open .atlas database")?,
    );

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

    Ok(())
}
