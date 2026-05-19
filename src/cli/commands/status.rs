//! `atlas status` — display project indexing status.

use crate::db::Store;
use anyhow::Context;
use std::path::Path;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);

    // Check if .atlas/ exists
    let atlas_dir = root.join(".atlas");
    if !atlas_dir.is_dir() {
        println!("No Atlas database found in {}", root.display());
        println!("Run `atlas init` to initialize this project.");
        return Ok(());
    }

    let store = Store::open(root).context("Failed to open Atlas database")?;
    let stats = store.get_stats().context("Failed to read database stats")?;

    println!("Atlas Project Status");
    println!("====================");
    println!("  Project root:    {}", root.display());
    println!("  Database:        {}/atlas.db", atlas_dir.display());
    println!("  SQLite version:  {}", stats.sqlite_version);
    println!("  Schema version:  v{}", crate::db::CURRENT_SCHEMA_VERSION);
    println!();
    println!("  Files indexed:   {}", stats.total_files);
    println!("  Symbols:         {}", stats.total_symbols);
    println!("  References:      {}", stats.total_references);
    println!("    - unresolved:  {}", stats.unresolved_references);
    println!("  Edges:           {}", stats.total_edges);

    // Show language breakdown
    if !stats.files_by_language.is_empty() {
        println!();
        println!("  By Language:");
        for (lang, count) in &stats.files_by_language {
            println!("    {:<14} {} files", lang, count);
        }
    }

    // Show symbol kind breakdown
    if !stats.symbols_by_kind.is_empty() {
        println!();
        println!("  By Symbol Kind:");
        for (kind, count) in &stats.symbols_by_kind {
            println!("    {:<14} {}", kind, count);
        }
    }

    // List indexed files if any
    if stats.total_files > 0 && stats.total_files <= 20 {
        let files = store.list_files().context("Failed to list files")?;
        println!();
        println!("  Indexed files:");
        for f in &files {
            let lang = f.language.as_str();
            println!("    [{}] {}", lang, f.path);
        }
    } else if stats.total_files > 20 {
        println!();
        println!("  ({} files indexed. Use `atlas files` to list them.)", stats.total_files);
    } else {
        println!();
        println!("  (No files indexed yet. Run `atlas index` to index your codebase.)");
    }

    Ok(())
}
