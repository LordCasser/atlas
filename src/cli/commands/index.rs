//! `atlas index` command.

use crate::db::Store;
use crate::extraction::LanguageRegistry;
use crate::types::Language;
use anyhow::Context;
use std::path::Path;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);

    // Open or create store
    let store = Store::open(root).context("Failed to open Atlas database")?;
    store
        .init_schema()
        .context("Failed to initialize schema")?;

    // Detect and load language grammars for files
    let languages = detect_project_languages(root);
    if languages.is_empty() {
        anyhow::bail!("No recognizable source files found in {}", root.display());
    }

    let _registry = LanguageRegistry::new(&languages)
        .context("Failed to load language grammars")?;

    println!("Languages detected: {:?}", languages);
    println!("Indexing project: {}", project);
    println!();

    // TODO: Walk files, parse with tree-sitter, run LanguageAdapter queries,
    //        collect FileFacts, store_file_facts.
    //        This will be fully implemented in M2 Query Extraction.
    println!("(Indexing stub -- full implementation in M2)");

    // Show current stats
    let stats = store.get_stats()?;
    println!();
    println!("Database status:");
    println!("  Files:    {}", stats.total_files);
    println!("  Symbols:  {}", stats.total_symbols);
    println!("  Edges:    {}", stats.total_edges);

    Ok(())
}

/// Walk the project root and detect what languages are present.
fn detect_project_languages(root: &Path) -> Vec<Language> {
    let mut langs = Vec::new();
    let mut seen = std::collections::HashSet::new();

    fn walk(dir: &Path, langs: &mut Vec<Language>, seen: &mut std::collections::HashSet<Language>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    // Skip hidden dirs and common non-source dirs
                    if name.starts_with('.')
                        || name == "node_modules"
                        || name == "target"
                        || name == "__pycache__"
                        || name == "venv"
                        || name == ".git"
                    {
                        continue;
                    }
                    walk(&path, langs, seen);
                } else if let Some(lang) = Language::from_path(&path) {
                    if seen.insert(lang) {
                        langs.push(lang);
                    }
                }
            }
        }
    }

    walk(root, &mut langs, &mut seen);
    langs
}
