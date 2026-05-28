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

/// Quick check: does the directory (recursively) contain source files we can recognize?
/// Stops at first match to avoid unnecessary directory walking.
fn has_source_files(root: &std::path::Path) -> bool {
    fn walk(dir: &std::path::Path, depth: u32) -> bool {
        // Cap depth at 20 to prevent infinite recursion on symlink cycles.
        if depth > 20 {
            return false;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if Language::from_path(&path).is_some() {
                        return true;
                    }
                } else if path.is_dir() {
                    // Avoid traversing hidden / build directories for performance.
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with('.') || name == "node_modules" || name == "target" || name == "build" {
                            continue;
                        }
                    }
                    if walk(&path, depth + 1) {
                        return true;
                    }
                }
            }
        }
        false
    }
    walk(root, 0)
}
