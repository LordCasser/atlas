//! `atlas init` — create `.atlas/` directory and initialize the database.

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_engine::Language;
use atlas_engine::discovery::{DiscoveryConfig, discover_files};

/// Threshold for suggesting manifest mode on first index.
const LARGE_PROJECT_THRESHOLD: usize = 5_000;

pub fn run(project: &str) -> anyhow::Result<()> {
    let ctx = CommandContext::open(project, DbMode::InitOrCreate)?;
    let ws = &ctx.workspace;

    // Validate it looks like a code project
    let has_code = has_source_files(ws.root());
    if !has_code {
        eprintln!(
            "Warning: No recognizable source files found in {}. \
             Is this a code project?",
            ws.root().display()
        );
    }

    println!("Atlas initialized successfully!");
    println!("  Database: {}/atlas.db", ws.atlas_dir().display());

    // Quick file count (git-aware, no parsing)
    let config = DiscoveryConfig::default();
    if let Ok(files) = discover_files(ws.root(), &config) {
        println!("  Files:    {} source files detected", files.len());
        if files.len() > LARGE_PROJECT_THRESHOLD {
            println!();
            println!("  ⚡ Large project detected! For a fast first pass:");
            println!("    atlas index --analysis manifest");
            println!("  This extracts top-level symbols in seconds.");
            println!("  Full analysis is triggered on-demand when you query.");
        }
    }

    // Show loaded language support
    let store_stats = ctx
        .store
        .get_stats()
        .context("Failed to read database stats")?;
    println!("  SQLite:   {}", store_stats.sqlite_version);
    println!("  Atlas version: {}", env!("CARGO_PKG_VERSION"));

    Ok(())
}

/// Quick check: does the directory contain source files we can recognize?
fn has_source_files(root: &std::path::Path) -> bool {
    let mut found = false;

    // Walk up to 100 entries deep, stop early on first match
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if Language::from_path(&path).is_some() {
                    found = true;
                    break;
                }
            }
            if path.is_dir() {
                // Shallow recursive into first-level subdirs
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub in sub_entries.flatten() {
                        let sub_path = sub.path();
                        if sub_path.is_file() {
                            if Language::from_path(&sub_path).is_some() {
                                found = true;
                                break;
                            }
                        }
                    }
                }
            }
            if found {
                break;
            }
        }
    }
    found
}
