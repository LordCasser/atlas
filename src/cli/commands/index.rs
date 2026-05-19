//! `atlas index` command — walk project tree (git-aware), extract facts, resolve references.

use crate::db::Store;
use crate::extraction::{self, LanguageRegistry};
use crate::sync::discovery::{discover_files, DiscoveryConfig};
use crate::types::Language;
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = Path::new(project);

    // Open or create store
    let store = Store::open(root).context("Failed to open Atlas database")?;
    store.init_schema().context("Failed to initialize schema")?;

    // Discover source files using git ls-files (or fallback walk)
    let config = DiscoveryConfig::default();
    let files = discover_files(root, &config)
        .context("Failed to discover source files")?;

    if files.is_empty() {
        anyhow::bail!("No recognizable source files found in {}", root.display());
    }

    // Detect required languages from discovered files
    let languages: Vec<Language> = files
        .iter()
        .filter_map(|p| Language::from_path(p))
        .fold(Vec::new(), |mut acc, lang| {
            if !acc.contains(&lang) { acc.push(lang); }
            acc
        });

    let _registry = LanguageRegistry::new(&languages)
        .context("Failed to load language grammars")?;

    println!("Languages detected: {:?}", languages);
    println!("Indexing project: {} ({} files discovered)", root.display(), files.len());
    println!();

    // Walk discovered files and extract
    let ext_files = index_discovered_files(root, &store, &files)?;
    println!("\nIndexed {} files.", ext_files);

    // Resolve all references
    println!("Resolving references...");
    let store = Arc::new(store);
    let resolver = crate::resolution::ReferenceResolver::new(Arc::clone(&store));
    let stats = resolver.resolve_all()?;

    println!("  Total references:   {}", stats.total_refs);
    println!("  Resolved:           {}", stats.resolved);
    println!("  Unresolved:         {}", stats.unresolved);
    println!("  Edges created:     {}", stats.edges_created);
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

    // Record indexing timestamp for incremental sync
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    store.set_metadata("last_index_time", &now)?;
    store.set_metadata("last_index_root", &root.display().to_string())?;

    Ok(())
}

/// Process each discovered file for extraction.
fn index_discovered_files(root: &Path, store: &Store, files: &[PathBuf]) -> anyhow::Result<usize> {
    let mut count = 0;

    for rel_path in files {
        let abs_path = root.join(rel_path);
        let lang = match Language::from_path(rel_path) {
            Some(l) => l,
            None => continue,
        };

        match process_one_file(&abs_path, root, lang, store) {
            Ok(()) => {
                count += 1;
                println!("  [{}] {}", count, rel_path.display());
            }
            Err(e) => {
                eprintln!(
                    "  Warning: {} — {:#}",
                    rel_path.display(),
                    e
                );
            }
        }
    }

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
