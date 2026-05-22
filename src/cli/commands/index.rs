//! `atlas index` command — walk project tree (git-aware), extract facts, resolve references.
//!
//! ## Design
//! - **Phase 1 (parallel)**: Extract all files using Rayon — CPU-bound, no SQLite access.
//! - **Phase 2 (sequential)**: Insert extracted facts into the store — SQLite single-writer.
//! - **Phase 3**: Resolve all references (batch).
//!
//! This two-phase approach avoids SQLite lock contention while maximizing CPU utilization
//! during the extraction phase (typically 70-80% of total index time).
//!
//! ## P0: Phase timing & per-language metrics
//! Every phase is wall-clock timed. Per-language file counts, extraction times,
//! and failure counts are aggregated.
//!
//! ## P1: Hash-based dirty-set incremental index
//! Before extraction, compute current file content hashes and compare against
//! the database. Clean (unchanged) files are skipped. Only dirty (new or modified)
//! files are re-extracted. Deleted files (in DB but not on disk) are cleaned up.

use crate::db::Store;
use crate::extraction::{self, LanguageRegistry, ParseWorkerPool, WorkerConfig};
use crate::extraction::frontend::LanguageFrontend;
use crate::sync::discovery::{DiscoveryConfig, discover_files};
use crate::types::FailureCategory;
use crate::types::Language;
use crate::types::{PerLanguageStats, PhaseTimer, PhaseTiming, PhaseTimings};
use anyhow::Context;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Result of extracting a single file — carries language for per-language stats.
struct ExtractedFile {
    rel_path: PathBuf,
    lang: Language,
    facts: crate::types::FileFacts,
}

/// Result of the hash-check phase: dirty set + deletion set.
struct HashCheckResult {
    /// Files that need extraction (new + modified).
    dirty: Vec<PathBuf>,
    /// Count of clean (unchanged) files.
    clean_count: usize,
    /// Files in DB but no longer on disk — need cleanup.
    deleted: Vec<PathBuf>,
}

pub fn run(project: &str, include: Option<&str>, exclude: Option<&str>) -> anyhow::Result<()> {
    let root = Path::new(project);
    let pipeline_start = Instant::now();
    let mut phase_timings = PhaseTimings::new();

    // ── Phase: Open store ──────────────────────────────────────────────────
    let _store_timer = PhaseTimer::start("Open store");
    let store = Store::open(root).context("Failed to open Atlas database")?;
    store.init_schema().context("Failed to initialize schema")?;

    // ── Phase: Discovery ───────────────────────────────────────────────────
    let disc_timer = PhaseTimer::start("Discovery");
    let mut config = DiscoveryConfig::default();
    if let Some(pat) = include {
        config.include_patterns = vec![pat.to_string()];
    }
    if let Some(pat) = exclude {
        config.exclude_patterns = vec![pat.to_string()];
    }
    let discovered = discover_files(root, &config).context("Failed to discover source files")?;

    if discovered.is_empty() {
        anyhow::bail!("No recognizable source files found in {}", root.display());
    }
    let total = discovered.len();
    let disc_timing = disc_timer.items(total as u64).finish();
    phase_timings.push(disc_timing);

    // ── Phase: Hash check (P1) ─────────────────────────────────────────────
    // Compute current hashes in parallel, compare with DB, classify files.
    let hash_timer = PhaseTimer::start("Hash check");
    let hash_result = build_dirty_set(&store, &discovered, root)?;
    let dirty = &hash_result.dirty;
    let reused = hash_result.clean_count;
    let hash_timing = hash_timer
        .items(dirty.len() as u64)
        .note(format!("{} reused, {} dirty", reused, dirty.len()))
        .finish();
    phase_timings.push(hash_timing);

    // ── Phase: Delete stale data for deleted files ─────────────────────────
    if !hash_result.deleted.is_empty() {
        let del_timer = PhaseTimer::start("Delete stale");
        for rel_path in &hash_result.deleted {
            let file_id = crate::types::FileId::generate(&rel_path.to_string_lossy());
            let _ = store.delete_edges_for_file_references(&file_id);
            let _ = store.delete_file_data(&file_id);
        }
        let del_timing = del_timer.items(hash_result.deleted.len() as u64).finish();
        phase_timings.push(del_timing);
    }

    // Detect required languages from dirty files (not all discovered)
    let languages: Vec<Language> =
        dirty
            .iter()
            .filter_map(|p| Language::from_path(p))
            .fold(Vec::new(), |mut acc, lang| {
                if !acc.contains(&lang) {
                    acc.push(lang);
                }
                acc
            });

    // ── Phase: Language init ───────────────────────────────────────────────
    let lang_timer = PhaseTimer::start("Language init");
    let _registry =
        LanguageRegistry::new(&languages).context("Failed to load language grammars")?;
    // P2: Build frontend cache — one LanguageFrontend per language, reused across all files.
    let frontend_cache: HashMap<Language, LanguageFrontend> = languages
        .iter()
        .filter_map(|&lang| {
            extraction::create_frontend(lang).map(|fe| (lang, fe))
        })
        .collect();
    let lang_timing = lang_timer.items(languages.len() as u64).finish();
    phase_timings.push(lang_timing);

    println!(
        "Languages detected: {:?}",
        languages.iter().map(|l| l.as_str()).collect::<Vec<_>>()
    );
    println!(
        "Indexing project: {} ({} files discovered)",
        root.display(),
        total
    );
    if reused > 0 {
        println!(
            "  {} dirty / {} reused (unchanged files skipped)",
            dirty.len(),
            reused
        );
    }
    if !hash_result.deleted.is_empty() {
        println!("  {} deleted files cleaned up", hash_result.deleted.len());
    }
    println!();

    // ── Phase 1: Parallel extraction (dirty files only) ────────────────────
    let extract_start = Instant::now();
    let pool = ParseWorkerPool::new(WorkerConfig::default());
    let dirty_total = dirty.len();
    let extracted_count = AtomicUsize::new(0);
    let per_lang_mutex = Mutex::new(PerLanguageStats::new());
    let fc = &frontend_cache; // P2: shared reference to cached frontends

    let results: Vec<_> = dirty
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let lang = Language::from_path(rel_path)?;

            // P2: look up cached frontend instead of creating per-file
            let frontend = fc.get(&lang)?;

            // Time per-file extraction
            let file_start = Instant::now();
            let result = extract_one_with_frontend(&pool, &abs_path, root, lang, frontend);
            let extract_ms = file_start.elapsed().as_millis() as u64;

            let count = extracted_count.fetch_add(1, Ordering::Relaxed);
            if count % 20 == 0 || count == dirty_total - 1 {
                eprint!("\r  Extracting: {}/{} ", count + 1, dirty_total);
            }

            // Record per-language stats (thread-safe)
            let (facts_opt, failed, fail_cat) = match result {
                Ok(facts) => (Some(facts), false, None),
                Err(()) => (None, true, Some("extraction_error")),
            };
            {
                per_lang_mutex
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record_file(lang, extract_ms, failed, fail_cat);
            }

            facts_opt.map(|facts| ExtractedFile {
                rel_path: rel_path.clone(),
                lang,
                facts,
            })
        })
        .collect();

    eprintln!(); // newline after progress
    let extract_elapsed = extract_start.elapsed();

    let extracted = results;
    let extracted_count = extracted.len();
    let avg_ms = if extracted_count > 0 {
        extract_elapsed.as_millis() as u64 / extracted_count as u64
    } else {
        0
    };

    // Collect per-language stats
    let lang_stats = per_lang_mutex
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    let mut per_lang = lang_stats;

    let extract_timing = PhaseTiming {
        phase: "Parse/extract".to_string(),
        duration_ms: extract_elapsed.as_millis() as u64,
        items: Some(extracted_count as u64),
        note: Some(format!("avg {}ms/file", avg_ms)),
    };
    phase_timings.push(extract_timing);

    // ── Phase: Clean stale facts for modified files ─────────────────────────
    // When re-indexing modified files, old rows (symbols, references,
    // dataflow, CFG, callsites) must be removed before inserting new
    // facts.  INSERT OR REPLACE only handles primary-key conflicts;
    // rows whose source code disappeared from the file persist as
    // "ghost" facts.  `delete_files_batch` uses FOREIGN KEY CASCADE
    // to wipe all related rows.
    {
        let clean_timer = PhaseTimer::start("Clean stale");
        let file_ids: Vec<_> = extracted.iter().map(|ef| ef.facts.file.file_id).collect();
        if let Err(e) = store.delete_files_batch(&file_ids) {
            tracing::warn!("Failed to clean stale facts for dirty files: {:#}", e);
        }
        let clean_timing = clean_timer.items(file_ids.len() as u64).finish();
        phase_timings.push(clean_timing);
    }

    // ── Phase 2: Batch insertion (P3: single transaction per chunk) ──────
    let insert_timer = PhaseTimer::start("DB write");
    let mut insert_failures = 0usize;
    // P3: batch-insert in chunks of 200 files to balance transaction size
    // vs. per-transaction overhead.
    const BATCH_SIZE: usize = 200;
    for chunk in extracted.chunks(BATCH_SIZE) {
        let facts: Vec<_> = chunk.iter().map(|ef| ef.facts.clone()).collect();
        if let Err(e) = store.insert_file_facts_batch(&facts) {
            // Fall back to per-file insert on batch failure
            for ef in chunk {
                if let Err(e2) = store.insert_file_facts(&ef.facts) {
                    pool.push_failure(
                        &ef.rel_path.to_string_lossy(),
                        FailureCategory::QueryError,
                        format!("Insert failed: {:#}", e2),
                    );
                    insert_failures += 1;
                    per_lang.record_file(ef.lang, 0, true, Some("db_insert_error"));
                }
            }
            // Log the batch error but continue
            tracing::warn!("Batch insert failed ({} files): {:#}", chunk.len(), e);
        }
    }
    let insert_timing = insert_timer
        .items((extracted_count - insert_failures) as u64)
        .finish();
    phase_timings.push(insert_timing);

    // Build index report from pool
    let mut index_report = pool.into_report(dirty_total, 0);
    index_report.files_indexed = extracted_count.saturating_sub(insert_failures);

    // ── Phase 3: Resolve all references ───────────────────────────────────
    println!("\nResolving references...");
    let res_timer = PhaseTimer::start("Resolution");
    let store = Arc::new(store);
    // P2: Load tsconfig.json path aliases if present
    let path_alias =
        crate::resolution::PathAliasResolver::from_tsconfig(&root.join("tsconfig.json"))
            .unwrap_or_else(crate::resolution::PathAliasResolver::empty);

    // P2: Detect tsconfig.json change and invalidate all import resolutions
    // if the path alias config differs from the previous run.
    //
    // jsconfig.json is NOT checked for invalidation because the resolver only
    // loads tsconfig.json for path alias resolution.  JS projects requiring
    // path aliases should use tsconfig.json (supported by tsc/TypeScript parser).
    {
        for name in &["tsconfig.json"] {
            let config_path = root.join(name);
            let current_hash = std::fs::read(&config_path)
                .ok()
                .map(|c| blake3::hash(&c).to_hex().to_string());
            let meta_key = format!("{}_hash", name);
            let prev_hash = store.get_metadata(&meta_key).ok().flatten();

            match (&prev_hash, &current_hash) {
                (Some(prev), Some(curr)) if prev == curr => {
                    // Unchanged
                    continue;
                }
                (None, None) => {
                    // No config file before or now
                    continue;
                }
                _ => {
                    // Config appeared, disappeared, or changed — invalidate
                    let inv_refs = store.invalidate_all_references().unwrap_or(0);
                    let inv_edges = store.delete_all_edges().unwrap_or(0);
                    tracing::info!(
                        "{} changed — invalidated {} references and {} edges for re-resolution",
                        name, inv_refs, inv_edges
                    );
                    match &current_hash {
                        Some(hash) => {
                            let _ = store.set_metadata(&meta_key, hash);
                        }
                        None => {
                            // Config deleted — clear stored hash to avoid
                            // repeated invalidation on every run
                            let _ = store.delete_metadata(&meta_key);
                        }
                    }
                }
            }
        }
    }

    let mut resolver =
        crate::resolution::ReferenceResolver::with_path_alias(Arc::clone(&store), path_alias);
    let (resolved, stats) = resolver.resolve_all()?;
    let res_elapsed = res_timer
        .items(stats.total_refs as u64)
        .finish()
        .duration_ms;
    phase_timings.push(PhaseTiming {
        phase: "Resolution".to_string(),
        duration_ms: res_elapsed,
        items: Some(stats.total_refs as u64),
        note: Some(format!("{} resolved", stats.resolved)),
    });

    index_report.references_total = stats.total_refs;
    index_report.references_resolved = stats.resolved;

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

    // ── Phase 3b: Build edges ────────────────────────────────────────────
    println!("\nBuilding edges...");
    let edge_timer = PhaseTimer::start("Graph build");
    let builder = crate::graph::GraphBuilder::new(Arc::clone(&store));
    let build_stats = builder.build_all(&resolved);
    let edge_elapsed = edge_timer
        .items(build_stats.edges_built as u64)
        .finish()
        .duration_ms;
    phase_timings.push(PhaseTiming {
        phase: "Graph build".to_string(),
        duration_ms: edge_elapsed,
        items: Some(build_stats.edges_built as u64),
        note: None,
    });
    println!("  Edges built:         {}", build_stats.edges_built);

    // Show final stats
    let db_stats = store.get_stats()?;
    println!();
    println!("Database status:");
    println!("  Files:    {}", db_stats.total_files);
    println!("  Symbols:  {}", db_stats.total_symbols);
    println!("  Edges:    {}", db_stats.total_edges);

    // ── Record indexing metadata ──────────────────────────────────────────
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    store.set_metadata("last_index_time", &now)?;
    store.set_metadata("last_index_root", &root.display().to_string())?;

    // ── Phase timing summary ─────────────────────────────────────────────
    let total_elapsed = pipeline_start.elapsed();
    phase_timings.set_total(total_elapsed);

    index_report.duration_ms = total_elapsed.as_millis() as u64;
    index_report.phase_timings = phase_timings.clone();
    index_report.per_language = per_lang.clone();

    print_phase_timings(&phase_timings);
    if !per_lang.is_empty() {
        print_per_language_stats(&per_lang);
    }

    Ok(())
}

// ── P1: Hash-based dirty set computation ──────────────────────────────────

/// Build the set of files that need re-extraction.
///
/// 1. Compute current content hashes for discovered files (Rayon parallel).
/// 2. Query DB for previously indexed file hashes.
/// 3. Classify each file: New, Dirty, or Clean.
/// 4. Detect deleted files (in DB but not on disk).
fn build_dirty_set(
    store: &Store,
    discovered: &[PathBuf],
    root: &Path,
) -> anyhow::Result<HashCheckResult> {
    // ── 1. Parallel hash computation for discovered files ──────────────────
    let current_hashes: HashMap<String, String> = discovered
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let content = std::fs::read(&abs_path).ok()?;
            let hash = blake3::hash(&content).to_hex().to_string();
            Some((rel_path.to_string_lossy().to_string(), hash))
        })
        .collect();

    // ── 2. Query DB for existing file hashes ──────────────────────────────
    let db_files = store.list_files().unwrap_or_default();
    let db_hashes: HashMap<String, String> = db_files
        .iter()
        .map(|f| (f.path.clone(), f.content_hash.clone()))
        .collect();
    let db_paths: HashSet<String> = db_hashes.keys().cloned().collect();

    // ── 3. Classify each discovered file ──────────────────────────────────
    let mut dirty = Vec::new();
    let mut clean_count = 0usize;
    let discovered_set: HashSet<String> = current_hashes.keys().cloned().collect();

    for rel_path in discovered {
        let key = rel_path.to_string_lossy().to_string();
        match db_hashes.get(&key) {
            None => {
                // Not in DB — new file
                dirty.push(rel_path.clone());
            }
            Some(db_hash) => {
                if let Some(curr_hash) = current_hashes.get(&key) {
                    if curr_hash == db_hash {
                        // Content unchanged — skip
                        clean_count += 1;
                    } else {
                        // Content changed — re-extract
                        dirty.push(rel_path.clone());
                    }
                } else {
                    // File failed to hash (was deleted between discovery and here?)
                    // Treat as new and try to extract.
                    dirty.push(rel_path.clone());
                }
            }
        }
    }

    // ── 4. Detect deleted files (in DB but no longer on disk) ─────────────
    let deleted: Vec<PathBuf> = db_paths
        .difference(&discovered_set)
        .map(|p| PathBuf::from(p))
        .collect();

    Ok(HashCheckResult {
        dirty,
        clean_count,
        deleted,
    })
}

// ── Extraction helpers ────────────────────────────────────────────────────

/// Extract a single file using a cached LanguageFrontend (P2: avoids per-file frontend creation).
fn extract_one_with_frontend(
    pool: &ParseWorkerPool,
    path: &Path,
    root: &Path,
    _lang: Language,
    frontend: &LanguageFrontend,
) -> Result<crate::types::FileFacts, ()> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let relative = path.strip_prefix(root).unwrap_or(path);
            pool.push_failure(
                &relative.to_string_lossy(),
                FailureCategory::IoError,
                format!("Failed to read {}: {}", path.display(), e),
            );
            return Err(());
        }
    };

    let content_hash = blake3::hash(source.as_bytes()).to_hex();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let file_id = crate::types::FileId::generate(&relative.to_string_lossy());

    pool.extract_one(frontend, file_id, relative, &source, &content_hash)
        .map_err(|_| ())
}

// ── Timing output formatting ──────────────────────────────────────────────

fn print_phase_timings(timings: &PhaseTimings) {
    println!();
    println!("Phase timings:");
    println!("  {:<20} {:>8}  {}", "Phase", "Time", "Details");
    println!("  {:-<20} {:-<8}  {:-<20}", "", "", "");

    for t in &timings.phases {
        let time_str = format_duration(t.duration_ms);
        let details = format_phase_details(t);
        println!("  {:<20} {:>8}  {}", t.phase, time_str, details);
    }

    println!("  {:<20} {:>8}", "Total", format_duration(timings.total_ms));
}

fn format_duration(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    } else if ms >= 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms >= 1000 {
        format!("{}ms", ms)
    } else {
        format!("{}ms", ms)
    }
}

fn format_phase_details(t: &crate::types::PhaseTiming) -> String {
    let mut parts = Vec::new();
    if let Some(items) = t.items {
        parts.push(format!("{} items", items));
    }
    if let Some(ref note) = t.note {
        parts.push(note.clone());
    }
    parts.join(", ")
}

fn print_per_language_stats(stats: &PerLanguageStats) {
    println!();
    println!("Per-language stats:");
    println!(
        "  {:<15} {:>6} {:>12}  {}",
        "Language", "Files", "Time", "Errors"
    );
    println!("  {:-<15} {:-<6} {:-<12}  {:-<20}", "", "", "", "");

    for (lang, entry) in &stats.languages {
        let err_detail = if entry.failures > 0 {
            let cats: Vec<String> = entry
                .failures_by_category
                .iter()
                .map(|(c, n)| format!("{n} {c}"))
                .collect();
            format!("{} ({})", entry.failures, cats.join(", "))
        } else {
            "0".to_string()
        };

        println!(
            "  {:<15} {:>6} {:>12}  {}",
            lang,
            entry.file_count,
            format_duration(entry.extract_ms),
            err_detail
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_ms() {
        assert_eq!(format_duration(120), "120ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(8500), "8500ms");
        assert_eq!(format_duration(1200), "1200ms");
    }

    #[test]
    fn format_duration_long() {
        assert_eq!(format_duration(10_500), "10.5s");
        assert_eq!(format_duration(65_000), "1m 5s");
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration(0), "0ms");
    }
}
