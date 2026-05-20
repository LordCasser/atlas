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
use crate::extraction::{self, LanguageRegistry, ParseWorkerPool, WorkerConfig};
use crate::sync::discovery::{discover_files, DiscoveryConfig};
use crate::types::Language;
use crate::types::FailureCategory;
use anyhow::Context;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
    // Use ParseWorkerPool for panic isolation, size checks, and error tracking.
    let pool = ParseWorkerPool::new(WorkerConfig::default());
    let total = files.len();
    let extracted_count = AtomicUsize::new(0);

    let results: Vec<_> = files
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let lang = Language::from_path(rel_path)?;

            // Extract with pool (CPU-bound, no SQLite access)
            let result = extract_one_with_pool(&pool, &abs_path, root, lang);

            let count = extracted_count.fetch_add(1, Ordering::Relaxed);
            if count % 100 == 0 || count == total - 1 {
                eprint!("\r  Extracting: {}/{} ", count + 1, total);
            }

            match result {
                Ok(facts) => {
                    Some(ExtractedFile {
                        rel_path: rel_path.clone(),
                        facts,
                    })
                }
                Err((category, msg)) => {
                    pool.push_failure(&rel_path.to_string_lossy(), category, msg);
                    None
                }
            }
        })
        .collect();

    eprintln!(); // newline after progress

    let extracted = results; // already filtered: successes only, failures recorded to pool

    // ── Phase 2: Sequential insertion ─────────────────────────────────────
    let mut insert_failures = 0usize;
    for ef in &extracted {
        if let Err(e) = store.insert_file_facts(&ef.facts) {
            pool.push_failure(
                &ef.rel_path.to_string_lossy(),
                FailureCategory::QueryError, // insertion failure → DB error
                format!("Insert failed: {:#}", e),
            );
            insert_failures += 1;
        }
    }

    // Build final report from pool
    let mut index_report = pool.into_report(total, 0 /* duration filled below */);
    index_report.files_indexed = extracted.len().saturating_sub(insert_failures);
    // references filled by Phase 3 resolution

    // ── Print index summary ───────────────────────────────────────────────
    if index_report.files_failed > 0 {
        let success_count = index_report.files_indexed;
        let pct = (index_report.files_failed as f64 / total as f64) * 100.0;
        println!(
            "\nIndexed {}/{} files ({:.1}% success)",
            success_count, total, 100.0 - pct
        );
        println!("  {} failed:", index_report.files_failed);
        for (cat, count) in &index_report.failures_by_category {
            println!("    {}: {}", cat, count);
        }
        if index_report.files_skipped > 0 {
            println!("  {} skipped (size limit)", index_report.files_skipped);
        }
    } else {
        println!("\nIndexed {}/{} files (100% success)", total, total);
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

/// Extract a single file using the parse worker pool.
///
/// Pre-extraction errors (no adapter, I/O) are returned as `Err((category, message))`.
/// Extraction errors are recorded internally by the pool.
fn extract_one_with_pool(
    pool: &ParseWorkerPool,
    path: &Path,
    root: &Path,
    lang: Language,
) -> Result<crate::types::FileFacts, (FailureCategory, String)> {
    let adapter = extraction::create_adapter(lang)
        .ok_or_else(|| (FailureCategory::QueryError, format!("No adapter available for {:?}", lang)))?;

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return Err((FailureCategory::IoError, format!("Failed to read {}: {}", path.display(), e))),
    };

    let content_hash = blake3::hash(source.as_bytes()).to_hex();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let file_id = crate::types::FileId::generate(&relative.to_string_lossy());

    // Delegate to pool: handles size check, panic isolation, and error recording
    pool.extract_one(adapter.as_ref(), file_id, relative, &source, &content_hash)
        .map_err(|err| {
            let category = err.category;
            (category, err.message)
        })
}
