//! `atlas init` — create `.atlas/` directory and initialize the database.

use crate::db::Store;
use anyhow::Context;
use std::path::Path;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);

    // Validate project root exists
    if !root.exists() {
        anyhow::bail!("Project root does not exist: {}", root.display());
    }
    if !root.is_dir() {
        anyhow::bail!("Not a directory: {}", root.display());
    }

    // Validate it looks like a code project (has at least one source file)
    let has_code = has_source_files(root);
    if !has_code {
        eprintln!(
            "Warning: No recognizable source files found in {}. \
             Is this a code project?",
            root.display()
        );
    }

    // Create .atlas/ directory and initialize DB
    let store = Store::open(root).context("Failed to open Atlas database")?;
    store
        .init_schema()
        .context("Failed to initialize database schema")?;

    let atlas_dir = root.join(".atlas");
    println!("Atlas initialized successfully!");
    println!("  Database: {}/atlas.db", atlas_dir.display());

    // Show loaded language support
    let store_stats = store.get_stats().context("Failed to read database stats")?;
    println!("  SQLite:   {}", store_stats.sqlite_version);
    println!("  Schema:   v{}", crate::db::CURRENT_SCHEMA_VERSION);

    Ok(())
}

/// Quick check: does the directory contain source files we can recognize?
fn has_source_files(root: &Path) -> bool {
    // Known extensions from the Atlas Language::from_extension map
    let known_extensions = [
        "ts", "mts", "cts", "js", "mjs", "cjs", "py", "pyi", "java", "c", "h", "cpp", "cc",
        "cxx", "hpp", "hh", "hxx", "ets", "cj", "cangjie",
    ];
    let mut found = false;

    // Walk up to 100 entries deep, stop early on first match
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if known_extensions.contains(&ext) {
                        found = true;
                        break;
                    }
                }
            }
            if path.is_dir() {
                // Shallow recursive into first-level subdirs
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub in sub_entries.flatten() {
                        let sub_path = sub.path();
                        if sub_path.is_file() {
                            if let Some(ext) = sub_path.extension().and_then(|e| e.to_str()) {
                                if known_extensions.contains(&ext) {
                                    found = true;
                                    break;
                                }
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
