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

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_engine::LanguageFrontend;
use atlas_engine::{self, LanguageRegistry, ParseWorkerPool, WorkerConfig};
use atlas_engine::FileLock;
use atlas_engine::discovery::{DiscoveryConfig, discover_files};
use atlas_engine::ExtractionError;
use atlas_engine::FailureCategory;
use atlas_engine::Language;
use atlas_engine::{PerLanguageStats, PhaseTimer, PhaseTiming, PhaseTimings};
use atlas_engine::SourcePath;
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
    facts: atlas_engine::FileFacts,
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
    // ── Phase: Open store ──────────────────────────────────────────────────
    let _store_timer = PhaseTimer::start("Open store");
    let ctx = CommandContext::open(project, DbMode::CreateOrOpenReadWrite)?;
    let _lock = FileLock::acquire(&ctx.store)
        .context("Another atlas process is indexing this project. Wait for it to finish, or stop the other process first.")?;
    let root = ctx.root.as_path();
    let pipeline_start = Instant::now();
    let mut phase_timings = PhaseTimings::new();

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
    let disc_ms = disc_timing.duration_ms;
    phase_timings.push(disc_timing);
    tracing::info!(phase = "discovery", files = total, duration_ms = disc_ms,);

    // ── Phase: Hash check (P1) ─────────────────────────────────────────────
    // Compute current hashes in parallel, compare with DB, classify files.
    let hash_timer = PhaseTimer::start("Hash check");
    let hash_result = build_dirty_set(&ctx.store, &discovered, root)?;
    let dirty = &hash_result.dirty;
    let reused = hash_result.clean_count;
    let hash_timing = hash_timer
        .items(dirty.len() as u64)
        .note(format!("{} reused, {} dirty", reused, dirty.len()))
        .finish();
    let hash_ms = hash_timing.duration_ms;
    phase_timings.push(hash_timing);
    tracing::info!(
        phase = "hash_check",
        dirty = dirty.len(),
        reused = reused,
        deleted = hash_result.deleted.len(),
        duration_ms = hash_ms,
    );

    // ── Phase: Delete stale data for deleted files ─────────────────────────
    if !hash_result.deleted.is_empty() {
        let del_timer = PhaseTimer::start("Delete stale");
        for rel_path in &hash_result.deleted {
            let sp = SourcePath::try_from_relative(&rel_path.to_string_lossy())
                .with_context(|| format!("invalid deleted path: {}", rel_path.display()))?;
            let file_id = atlas_engine::FileId::generate(sp.as_str());
            // Invalidate cross-file references BEFORE deleting symbols
            ctx.store
                .invalidate_references_to_symbols_in_file(&file_id)
                .with_context(|| format!("Failed to invalidate cross-refs for: {}", sp))?;
            ctx.store
                .delete_edges_for_file_references(&file_id)
                .with_context(|| format!("Failed to delete edges for stale file: {}", sp))?;
            ctx.store
                .delete_file_data(&file_id)
                .with_context(|| format!("Failed to delete stale file data: {}", sp))?;
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
    let _registry = match LanguageRegistry::new(&languages) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "Some language grammars are not compiled in: {:#}",
                e
            );
            tracing::warn!("Files in those languages will be skipped.");
            let available: Vec<Language> = languages
                .iter()
                .filter(|l| LanguageRegistry::new(&[**l]).is_ok())
                .copied()
                .collect();
            if available.is_empty() {
                anyhow::bail!(
                    "No language grammars available. Rebuild with --features to add language support."
                );
            }
            LanguageRegistry::new(&available)?
        }
    };
    // P2: Build frontend cache — one LanguageFrontend per language, reused across all files.
    let frontend_cache: HashMap<Language, LanguageFrontend> = languages
        .iter()
        .filter_map(|&lang| atlas_engine::create_frontend(lang).map(|fe| (lang, fe)))
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
                Err(ref e) => {
                    tracing::warn!(
                        file = %rel_path.display(),
                        lang = %lang.as_str(),
                        category = ?e.category,
                        message = %e.message,
                        "extraction failed"
                    );
                    (None, true, Some("extraction_error"))
                }
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
    tracing::info!(
        phase = "extract",
        files = extracted_count,
        failed = dirty_total.saturating_sub(extracted_count),
        duration_ms = extract_elapsed.as_millis() as u64,
        avg_ms = avg_ms,
    );

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
        // Invalidate cross-file references pointing into these files
        // before deleting symbols (prevents dangling resolved targets).
        for fid in &file_ids {
            let _ = ctx.store.invalidate_references_to_symbols_in_file(fid);
        }
        if let Err(e) = ctx.store.delete_files_batch(&file_ids) {
            return Err(anyhow::anyhow!(
                "Failed to clean stale facts before indexing: {:#}",
                e
            ));
        }
        let clean_timing = clean_timer.items(file_ids.len() as u64).finish();
        let clean_ms = clean_timing.duration_ms;
        phase_timings.push(clean_timing);
        tracing::info!(
            phase = "db_clean",
            files = file_ids.len(),
            duration_ms = clean_ms,
        );
    }

    // ── Phase 2: Batch insertion (P3: single transaction per chunk) ──────
    let insert_timer = PhaseTimer::start("DB write");
    let mut insert_failures = 0usize;
    // P3: batch-insert in chunks of 200 files to balance transaction size
    // vs. per-transaction overhead.
    const BATCH_SIZE: usize = 200;
    for chunk in extracted.chunks(BATCH_SIZE) {
        let facts: Vec<_> = chunk.iter().map(|ef| ef.facts.clone()).collect();
        if let Err(e) = ctx.store.insert_file_facts_batch(&facts) {
            // Fall back to per-file insert on batch failure
            for ef in chunk {
                if let Err(e2) = ctx.store.insert_file_facts(&ef.facts) {
                    pool.push_failure(
                        &ef.rel_path.to_string_lossy(),
                        FailureCategory::QueryError,
                        format!("Insert failed: {:#}", e2),
                    );
                    insert_failures += 1;
                    per_lang.record_file(ef.lang, 0, true, Some("db_insert_error"));
                }
            }
            // Log the batch error with file context for debugging.
            // FK constraint failures typically come from dataflow_edges
            // referencing data_nodes not in the batch, or symbol_edges
            // referencing unresolved symbols.
            let failed_paths: Vec<_> = chunk.iter()
                .map(|ef| ef.rel_path.to_string_lossy().to_string())
                .collect();
            let is_fk = format!("{:#}", e).contains("FOREIGN KEY");
            tracing::warn!(
                "Batch insert failed ({} files, FK={}): {:#}. Failed paths: {:?}",
                chunk.len(), is_fk, e, failed_paths
            );
        }
    }
    let insert_timing = insert_timer
        .items((extracted_count - insert_failures) as u64)
        .finish();
    let insert_ms = insert_timing.duration_ms;
    phase_timings.push(insert_timing);
    tracing::info!(
        phase = "db_insert",
        files = extracted_count.saturating_sub(insert_failures),
        failures = insert_failures,
        duration_ms = insert_ms,
    );

    // Build index report from pool
    let mut index_report = pool.into_report(dirty_total, 0);
    index_report.files_indexed = extracted_count.saturating_sub(insert_failures);

    // ── Phase 3: Resolve all references ───────────────────────────────────
    println!("\nResolving references...");
    let res_timer = PhaseTimer::start("Resolution");
    // P2: Load tsconfig.json or jsconfig.json path aliases if present
    let path_alias =
        atlas_engine::PathAliasResolver::from_tsconfig(&root.join("tsconfig.json"))
            .or_else(|| {
                atlas_engine::PathAliasResolver::from_jsconfig(&root.join("jsconfig.json"))
            })
            .unwrap_or_else(atlas_engine::PathAliasResolver::empty);

    let tsconfig_changed =
        atlas_engine::detect_config_change(&ctx.store, &root, &["tsconfig.json", "jsconfig.json"])?;
    if tsconfig_changed {
        let inv_refs = ctx
            .store
            .invalidate_all_references()
            .context("Failed to invalidate references for tsconfig change")?;
        let inv_edges = ctx
            .store
            .delete_all_edges()
            .context("Failed to delete edges for tsconfig change")?;
        tracing::info!(
            "tsconfig.json changed — invalidated {} references and {} edges for re-resolution",
            inv_refs,
            inv_edges
        );
    }

    let mut resolver =
        atlas_engine::ReferenceResolver::with_path_alias(Arc::clone(&ctx.store), path_alias);
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
    tracing::info!(
        phase = "resolution",
        total_refs = stats.total_refs,
        resolved = stats.resolved,
        unresolved = stats.unresolved,
        duration_ms = res_elapsed,
    );

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
    let builder = atlas_engine::GraphBuilder::new(Arc::clone(&ctx.store));
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
    tracing::info!(
        phase = "graph_build",
        edges_built = build_stats.edges_built,
        duration_ms = edge_elapsed,
    );
    if build_stats.edges_built != build_stats.edges_written {
        println!("  Edges built:         {} ({} written)", build_stats.edges_built, build_stats.edges_written);
        if !build_stats.warnings.is_empty() {
            println!("  Edge warnings:       {}", build_stats.warnings.first().unwrap_or(&String::new()));
        }
    } else {
        println!("  Edges built:         {}", build_stats.edges_built);
    }

    // Commit tsconfig hash baseline AFTER the full pipeline succeeded
    // (extraction + resolution + graph build).  Committing earlier would
    // leave the hash updated on partial failure, preventing retry.
    if tsconfig_changed {
        atlas_engine::commit_config_hashes(&ctx.store, &root, &["tsconfig.json"])?;
    }

    // Show final stats
    let db_stats = ctx.store.get_stats()?;
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
    ctx.store.set_metadata("last_index_time", &now)?;
    ctx.store
        .set_metadata("last_index_root", &root.display().to_string())?;

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
    store: &atlas_engine::Store,
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
            let key = SourcePath::try_from_relative(&rel_path.to_string_lossy()).ok()?;
            Some((key.as_str().to_string(), hash))
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
        let key = match SourcePath::try_from_relative(&rel_path.to_string_lossy()) {
            Ok(sp) => sp.as_str().to_string(),
            Err(_) => continue,
        };
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
) -> Result<atlas_engine::FileFacts, ExtractionError> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let relative = path.strip_prefix(root).unwrap_or(path);
            let rel_str = relative.to_string_lossy().to_string();
            let is_utf8_err = e.to_string().contains("UTF-8") || e.to_string().contains("utf8");
            let msg = if is_utf8_err {
                format!("Non-UTF-8 file skipped: {}", path.display())
            } else {
                format!("Failed to read {}: {}", path.display(), e)
            };
            pool.push_failure(&rel_str, FailureCategory::IoError, msg.clone());
            return Err(ExtractionError {
                file_path: rel_str,
                category: FailureCategory::IoError,
                message: msg,
            });
        }
    };

    let content_hash = blake3::hash(source.as_bytes()).to_hex();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let rel_str = relative.to_string_lossy().to_string();
    let sp = SourcePath::try_from_relative(&rel_str).map_err(|_| ExtractionError {
        file_path: rel_str.clone(),
        category: FailureCategory::IoError,
        message: format!("invalid source path: {}", relative.display()),
    })?;
    let file_id = atlas_engine::FileId::generate(sp.as_str());

    pool.extract_one(frontend, file_id, relative, &source, &content_hash)
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

fn format_phase_details(t: &atlas_engine::PhaseTiming) -> String {
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
