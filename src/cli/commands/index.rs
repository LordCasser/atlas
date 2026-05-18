//! `atlas index` command — walk project tree, extract facts, resolve references.

use crate::db::Store;
use crate::extraction::{self, LanguageRegistry};
use crate::types::Language;
use anyhow::Context;
use std::path::Path;
use std::sync::Arc;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);

    // Open or create store
    let store = Store::open(root).context("Failed to open Atlas database")?;
    store.init_schema().context("Failed to initialize schema")?;

    // Detect and load language grammars for files
    let languages = detect_project_languages(root);
    if languages.is_empty() {
        anyhow::bail!("No recognizable source files found in {}", root.display());
    }

    let registry = LanguageRegistry::new(&languages)
        .context("Failed to load language grammars")?;

    println!("Languages detected: {:?}", languages);
    println!("Indexing project: {}", root.display());
    println!();

    // Walk all source files and extract
    let ext_files = walk_and_index(root, &store, &registry)?;
    println!("\nIndexed {} files.", ext_files);

    // Resolve all references
    println!("Resolving references...");
    let store = Arc::new(store);
    let resolver = crate::resolution::ReferenceResolver::new(Arc::clone(&store));
    let stats = resolver.resolve_all()?;

    println!("  Total references:   {}", stats.total_refs);
    println!("  Resolved:           {}", stats.resolved);
    println!("  Unresolved:         {}", stats.unresolved);
    println!("  Edges promoted:     {}", stats.edges_promoted);
    if !stats.by_strategy.is_empty() {
        println!("  By strategy:");
        for (strat, count) in &stats.by_strategy {
            println!("    {}: {}", strat, count);
        }
    }

    // Show final stats
    let db_stats = store.get_stats()?;
    println!();
    println!("Database status:");
    println!("  Files:    {}", db_stats.total_files);
    println!("  Symbols:  {}", db_stats.total_symbols);
    println!("  Edges:    {}", db_stats.total_edges);

    Ok(())
}

/// Walk project files and extract FileFacts for each.
fn walk_and_index(root: &Path, store: &Store, _registry: &LanguageRegistry) -> anyhow::Result<usize> {
    let mut count = 0;

    fn walk(dir: &Path, root: &Path, store: &Store, count: &mut usize) -> anyhow::Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.')
                    || name == "node_modules"
                    || name == "target"
                    || name == "__pycache__"
                    || name == "venv"
                    || name == ".git"
                {
                    continue;
                }
                walk(&path, root, store, count)?;
            } else if let Some(lang) = Language::from_path(&path) {
                match process_one_file(&path, root, lang, store) {
                    Ok(()) => {
                        *count += 1;
                        println!("  [{}] {}", count, path.strip_prefix(root).unwrap_or(&path).display());
                    }
                    Err(e) => {
                        eprintln!(
                            "  Warning: {} — {}",
                            path.strip_prefix(root).unwrap_or(&path).display(),
                            e
                        );
                    }
                }
            }
        }
        Ok(())
    }

    walk(root, root, store, &mut count)?;
    Ok(count)
}

/// Extract a single file and insert its facts into the store.
fn process_one_file(path: &Path, root: &Path, lang: Language, store: &Store) -> anyhow::Result<()> {
    let adapter = extraction::create_adapter(lang)
        .ok_or_else(|| anyhow::anyhow!("No adapter available for {:?}", lang))?;

    let source = std::fs::read_to_string(path)
        .context("Failed to read source file")?;
    let content_hash = &blake3::hash(source.as_bytes()).to_hex();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let file_id = crate::types::FileId::generate(&relative.to_string_lossy());

    let facts = extraction::extract_file(
        adapter.as_ref(),
        file_id,
        relative,
        &source,
        content_hash,
    )?;

    store.insert_file_facts(&facts)
        .context("Failed to insert file facts")?;

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
