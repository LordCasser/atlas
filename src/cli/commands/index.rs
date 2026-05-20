//! `atlas index` command — walk project tree (git-aware), extract facts, resolve references.
//!
//! ## Design
//! - **Phase 1 (parallel)**: Extract all files using Rayon — CPU-bound, no SQLite access.
//! - **Phase 2 (sequential)**: Insert extracted facts into the store — SQLite single-writer.
//! - **Phase 3**: Resolve all references (batch).
//!
//! This two-phase approach avoids SQLite lock contention while maximizing CPU utilization
//! during the extraction phase (typically 70-80% of total index time).

use crate::db::Store;
use crate::extraction::{self, LanguageRegistry};
use crate::sync::discovery::{discover_files, DiscoveryConfig};
use crate::types::Language;
use anyhow::Context;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Categorized index failure for summary reporting.
#[derive(Debug)]
#[allow(dead_code)]
enum IndexFailure {
    /// No adapter compiled in for this language.
    NoAdapter(String),
    /// File I/O error (e.g. permission denied, encoding).
    IoError(String),
    /// Tree-sitter extraction error (parse failure, etc.).
    ExtractError(String),
    /// Database insertion error (FK violation, etc.).
    InsertError(String),
}

impl IndexFailure {
    fn category(&self) -> &'static str {
        match self {
            IndexFailure::NoAdapter(_) => "no_adapter",
            IndexFailure::IoError(_) => "io_error",
            IndexFailure::ExtractError(_) => "extract_error",
            IndexFailure::InsertError(_) => "insert_error",
        }
    }
}

/// Result of extracting a single file.
struct ExtractedFile {
    rel_path: PathBuf,
    facts: crate::types::FileFacts,
}

pub fn run(project: &str, include: Option<&str>, exclude: Option<&str>) -> anyhow::Result<()> {
    let root = Path::new(project);

    // Open or create store
    let store = Store::open(root).context("Failed to open Atlas database")?;
    store.init_schema().context("Failed to initialize schema")?;

    // Discover source files using git ls-files (or fallback walk)
    let mut config = DiscoveryConfig::default();
    if let Some(pat) = include {
        config.include_patterns = vec![pat.to_string()];
    }
    if let Some(pat) = exclude {
        config.exclude_patterns = vec![pat.to_string()];
    }
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
            if !acc.contains(&lang) {
                acc.push(lang);
            }
            acc
        });

    let _registry = LanguageRegistry::new(&languages)
        .context("Failed to load language grammars")?;

    println!(
        "Languages detected: {:?}",
        languages.iter().map(|l| l.as_str()).collect::<Vec<_>>()
    );
    println!(
        "Indexing project: {} ({} files discovered)",
        root.display(),
        files.len()
    );
    println!();

    // ── Phase 1: Parallel extraction ──────────────────────────────────────
    // Extract all files in parallel (CPU-bound). Collect results + failures.
    let total = files.len();
    let extracted_count = AtomicUsize::new(0);

    let results: Vec<_> = files
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let lang = Language::from_path(rel_path)?;

            // Extract (CPU-bound, no SQLite access)
            let result = extract_one_file(&abs_path, root, lang);

            let count = extracted_count.fetch_add(1, Ordering::Relaxed);
            if count % 100 == 0 || count == total - 1 {
                eprint!("\r  Extracting: {}/{} ", count + 1, total);
            }

            let rel = rel_path.clone();
            match result {
                Ok(facts) => Some(Ok(ExtractedFile {
                    rel_path: rel,
                    facts,
                })),
                Err(e) => Some(Err((rel, e))),
            }
        })
        .collect();

    eprintln!(); // newline after progress

    // Partition into successes and failures
    let mut extracted: Vec<ExtractedFile> = Vec::new();
    let mut failures: Vec<(PathBuf, anyhow::Error)> = Vec::new();
    for r in results {
        match r {
            Ok(f) => extracted.push(f),
            Err((p, e)) => failures.push((p, e)),
        }
    }

    // ── Phase 2: Sequential insertion ─────────────────────────────────────
    // Insert all extracted facts into the store (SQLite single-writer).
    let mut insert_failures: Vec<(PathBuf, IndexFailure)> = Vec::new();
    for ef in &extracted {
        match store.insert_file_facts(&ef.facts) {
            Ok(()) => {}
            Err(e) => {
                insert_failures.push((
                    ef.rel_path.clone(),
                    IndexFailure::InsertError(format!("{:#}", e)),
                ));
            }
        }
    }

    // Categorize extraction failures
    let mut all_failures: Vec<(PathBuf, IndexFailure)> = Vec::new();
    for (path, err) in failures {
        let err_str = format!("{:#}", err);
        let category = if err_str.contains("No adapter") {
            IndexFailure::NoAdapter(err_str)
        } else if err_str.contains("Failed to read") || err_str.contains("No such file") {
            IndexFailure::IoError(err_str)
        } else {
            IndexFailure::ExtractError(err_str)
        };
        all_failures.push((path, category));
    }
    all_failures.extend(insert_failures);

    let fail_count = all_failures.len();
    let success_count = extracted.len()
        - all_failures
            .iter()
            .filter(|(_, f)| matches!(f, IndexFailure::InsertError(_)))
            .count();

    // ── Print index summary ───────────────────────────────────────────────
    if fail_count > 0 {
        let pct = (fail_count as f64 / total as f64) * 100.0;
        println!(
            "\nIndexed {}/{} files ({:.1}% success)",
            success_count,
            total,
            100.0 - pct
        );

        // Group failures by category
        let mut by_category: std::collections::HashMap<&str, Vec<&PathBuf>> =
            std::collections::HashMap::new();
        for (path, failure) in &all_failures {
            by_category
                .entry(failure.category())
                .or_default()
                .push(path);
        }
        println!("  {} failed:", fail_count);
        for (cat, paths) in &by_category {
            println!("    {} ({}): {}", cat, paths.len(), paths.iter().take(5).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
            if paths.len() > 5 {
                println!("      ... and {} more", paths.len() - 5);
            }
        }
        // Print the first failure's error message for debugging
        if let Some((path, failure)) = all_failures.first() {
            let err_msg = match failure {
                IndexFailure::NoAdapter(m) => m.clone(),
                IndexFailure::IoError(m) => m.clone(),
                IndexFailure::ExtractError(m) => m.clone(),
                IndexFailure::InsertError(m) => m.clone(),
            };
            println!("\n  First error ({}):\n    {}", path.display(), err_msg.lines().next().unwrap_or(&err_msg));
        }
    } else {
        println!("\nIndexed {}/{} files (100% success)", success_count, total);
    }

    // ── Phase 3: Resolve all references (P2: two-step pipeline) ──────────
    println!("\nResolving references...");
    let store = Arc::new(store);
    let resolver = crate::resolution::ReferenceResolver::new(Arc::clone(&store));
    let (resolved, stats) = resolver.resolve_all()?;

    let resolution_rate = if stats.total_refs > 0 {
        (stats.resolved as f64 / stats.total_refs as f64) * 100.0
    } else {
        0.0
    };
    println!("  Total references:   {}", stats.total_refs);
    println!(
        "  Resolved:           {} ({:.1}%)",
        stats.resolved, resolution_rate
    );
    println!("  Unresolved:         {}", stats.unresolved);
    if !stats.by_strategy.is_empty() {
        println!("  By strategy:");
        for (strat, count) in &stats.by_strategy {
            println!("    {}: {}", strat, count);
        }
    }
    if !stats.warnings.is_empty() {
        println!("  Warnings ({}):", stats.warnings.len());
        for w in stats.warnings.iter().take(10) {
            println!("    - {}", w);
        }
        if stats.warnings.len() > 10 {
            println!("    ... and {} more", stats.warnings.len() - 10);
        }
    }

    // ── Phase 3b: Build edges from resolved references ───────────────────
    println!("\nBuilding edges...");
    let builder = crate::graph::GraphBuilder::new(Arc::clone(&store));
    let build_stats = builder.build_all(&resolved);
    println!("  Edges built:         {}", build_stats.edges_built);

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

/// Extract a single file (CPU-bound, no SQLite access).
fn extract_one_file(
    path: &Path,
    root: &Path,
    lang: Language,
) -> anyhow::Result<crate::types::FileFacts> {
    let adapter = extraction::create_adapter(lang)
        .ok_or_else(|| anyhow::anyhow!("No adapter available for {:?}", lang))?;

    let source = std::fs::read_to_string(path).context("Failed to read source file")?;
    let content_hash = blake3::hash(source.as_bytes()).to_hex();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let file_id = crate::types::FileId::generate(&relative.to_string_lossy());

    let facts = extraction::extract_file(
        adapter.as_ref(),
        file_id,
        relative,
        &source,
        &content_hash,
    )?;

    Ok(facts)
}
