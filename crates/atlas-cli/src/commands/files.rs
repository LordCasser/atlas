//! `atlas files` command — list indexed files with symbol counts.

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;

pub fn run(project: &str) -> anyhow::Result<()> {
    let ctx = CommandContext::open(project, DbMode::ExistingReadOnly)?;
    let stats = ctx
        .store
        .get_stats()
        .context("Failed to read database stats")?;

    if stats.total_files == 0 {
        println!("No files indexed. Run `atlas index` first.");
        return Ok(());
    }

    let files = ctx.store.list_files().context("Failed to list files")?;

    println!("Indexed Files ({})", files.len());
    println!("{:-<80}", "");
    println!("{:<5} {:<12} Path", "No.", "Language");
    println!("{:-<80}", "");

    for (i, f) in files.iter().enumerate() {
        println!("{:<5} {:<12} {}", i + 1, f.language.as_str(), f.path);
    }

    println!();
    println!(
        "Total: {} files, {} symbols, {} edges",
        stats.total_files, stats.total_symbols, stats.total_edges,
    );

    Ok(())
}
