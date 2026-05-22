//! `atlas files` command — list indexed files with symbol counts.

use atlas_db::Store;
use atlas_workspace::Workspace;
use anyhow::Context;

pub fn run(project: &str) -> anyhow::Result<()> {
    let ws = Workspace::open(std::path::Path::new(project))
        .with_context(|| format!("Invalid project path: {}", project))?;
    if !ws.db_path().is_file() {
        anyhow::bail!(
            "Not an initialized Atlas project. Run `atlas init {}` first.",
            project
        );
    }
    let store = Store::open_db(ws.db_path()).context("Failed to open Atlas database")?;
    let stats = store.get_stats().context("Failed to read database stats")?;

    if stats.total_files == 0 {
        println!("No files indexed. Run `atlas index` first.");
        return Ok(());
    }

    let files = store.list_files().context("Failed to list files")?;

    println!("Indexed Files ({})", files.len());
    println!("{:-<80}", "");
    println!("{:<5} {:<12} {}", "No.", "Language", "Path");
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
