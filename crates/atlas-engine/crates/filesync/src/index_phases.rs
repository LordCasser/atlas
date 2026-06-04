//! Composable, stateless indexing phases for filesync.
//!
//! Each phase is a pure function: it takes inputs and produces outputs
//! without mutating shared state.  Callers choose which phases to run
//! and how to compose them — the phases themselves know nothing about
//! progress callbacks, interrupts, or pipeline orchestration.
//!
//! # Typical composition (CLI full-index)
//!
//! ```ignore
//! let discovered  = phase_discover(root, &include, &exclude)?;
//! let frontends   = phase_init_frontends(&discovered)?;
//! phase_cleanup_stale(&store, &discovered)?;
//! let extracted   = phase_extract_serial(root, &discovered, &frontends, mode, None);
//! phase_write_batched(&store, &extracted, 500, 500, |_| {}, || false)?;
//! let graph       = phase_resolve_and_build(&store, root)?;
//! phase_materialize_annotations(&store)?;
//! phase_build_summaries(&store)?;
//! phase_finalize(&store, root, &[])?;
//! ```
//!
//! # Cancellable extraction (with per-file progress)
//!
//! ```ignore
//! let cancel = AtomicBool::new(false);
//! let extracted = phase_extract_parallel_cancellable(
//!     root, &files, &frontends, mode,
//!     None,                                      // on_progress (once at end)
//!     Some(&|completed, total| { ... }),         // on_file_progress (every 50 files)
//!     Some(&cancel),                             // cancel_token
//! );
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use db::Store;
use extraction::{
    ExtractionMode, LanguageFrontend, LanguageRegistry, ParseWorkerPool, WorkerConfig,
    create_frontend,
};
use graph::GraphBuilder;
use resolution::{PathAliasConfig, ReferenceResolver};
use types::{FileFacts, FileId, Language};

use crate::cleanup::{clean_stale_file_ids, clean_stale_file_paths, source_file_id};
use crate::dirty::{DirtySet, build_dirty_set};
use crate::discovery::{DiscoveryConfig, discover_files};

// ── Public types ───────────────────────────────────────────────────────

/// One successfully extracted file.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    /// Project-relative path.
    pub rel_path: PathBuf,
    /// Detected source language.
    pub language: Language,
    /// Extracted facts (symbols, references, dataflow, etc.).
    pub facts: FileFacts,
}

/// Statistics gathered during extraction.
#[derive(Debug, Clone, Default)]
pub struct ExtractionPhaseStats {
    /// Total files attempted (including skipped / failed).
    pub attempted: usize,
    /// Successfully extracted files.
    pub succeeded: usize,
    /// Failed extractions (read, parse, or normalisation errors).
    pub failed: usize,
    /// Total symbol count across all extracted files.
    pub symbols: usize,
}

/// Extraction result: extracted files + stats.
#[derive(Debug, Clone)]
pub struct ExtractedFiles {
    /// Successfully extracted file facts.
    pub items: Vec<ExtractedFile>,
    /// Extraction phase statistics.
    pub stats: ExtractionPhaseStats,
}

/// Result of reference resolution + graph edge building.
#[derive(Debug, Clone, Default)]
pub struct GraphResult {
    /// Number of resolved references.
    pub resolved: usize,
    /// Number of graph edges built.
    pub edges_built: usize,
}

/// Result of a batch DB write.
#[derive(Debug, Clone, Default)]
pub struct WriteBatchStats {
    /// Number of files successfully written.
    pub written: usize,
    /// Number of batch insert attempts that failed and fell back to single inserts.
    pub batch_failures: usize,
    /// Number of individual file insert failures.
    pub single_failures: usize,
}

// ── Phase 1: Discovery ─────────────────────────────────────────────────

/// Discover source files under `root` respecting include/exclude globs.
///
/// Returns project-relative paths.
pub fn phase_discover(root: &Path, include: &[String], exclude: &[String]) -> Result<Vec<PathBuf>> {
    let mut config = DiscoveryConfig::default();
    if !include.is_empty() {
        config.include_patterns = include.to_vec();
    }
    if !exclude.is_empty() {
        config.exclude_patterns = exclude.to_vec();
    }
    discover_files(root, &config).context("Failed to discover files")
}

// ── Phase 2: Dirty check ───────────────────────────────────────────────

/// Build dirty set via content-hash comparison.
///
/// Returns which discovered files need re-extraction vs can be reused.
pub fn phase_dirty_check(
    store: &Arc<Store>,
    discovered: &[PathBuf],
    root: &Path,
) -> Result<DirtySet> {
    build_dirty_set(store, discovered, root)
}

// ── Phase 3: Stale cleanup ─────────────────────────────────────────────

/// Delete stale facts for removed or changed files (by project-relative path).
pub fn phase_cleanup_stale(store: &Arc<Store>, paths: &[PathBuf]) -> Result<()> {
    clean_stale_file_paths(store, paths)
        .map(|_| ())
        .context("Failed to clean stale file paths")
}

/// Delete stale facts for specific file IDs before re-extraction.
pub fn phase_cleanup_file_ids(store: &Arc<Store>, file_ids: &[FileId]) -> Result<()> {
    clean_stale_file_ids(store, file_ids).context("Failed to clean stale file IDs")
}

// ── Phase 4: Frontend init ─────────────────────────────────────────────

/// Initialise language frontends for the languages present in `files`.
///
/// Loads tree-sitter grammars via [`LanguageRegistry`] and creates one
/// [`LanguageFrontend`] per detected language.
pub fn phase_init_frontends(files: &[PathBuf]) -> Result<HashMap<Language, LanguageFrontend>> {
    let languages: Vec<Language> =
        files
            .iter()
            .filter_map(|p| Language::from_path(p))
            .fold(Vec::new(), |mut acc, lang| {
                if !acc.contains(&lang) {
                    acc.push(lang);
                }
                acc
            });

    let _registry =
        LanguageRegistry::new(&languages).context("Failed to initialize language registry")?;

    Ok(languages
        .iter()
        .filter_map(|&lang| create_frontend(lang).map(|fe| (lang, fe)))
        .collect())
}

// ── Phase 5: Extraction (serial) ───────────────────────────────────────

/// Extract facts from files serially — no DB writes.
///
/// Returns intermediate [`ExtractedFiles`]; the caller chooses the write
/// strategy (single or batched).
pub fn phase_extract_serial(
    root: &Path,
    files: &[PathBuf],
    frontends: &HashMap<Language, LanguageFrontend>,
    mode: ExtractionMode,
    on_progress: Option<&dyn Fn(usize, usize)>,
) -> ExtractedFiles {
    let pool = ParseWorkerPool::new(WorkerConfig::default());
    let total = files.len();
    let mut items = Vec::new();
    let mut stats = ExtractionPhaseStats {
        attempted: 0,
        succeeded: 0,
        failed: 0,
        symbols: 0,
    };

    for (i, rel_path) in files.iter().enumerate() {
        let abs_path = root.join(rel_path);
        let lang = match Language::from_path(rel_path) {
            Some(l) => l,
            None => continue,
        };
        let frontend = match frontends.get(&lang) {
            Some(fe) => fe,
            None => continue,
        };

        stats.attempted += 1;
        match extract_one_index_file(&pool, &abs_path, root, frontend, &mode) {
            Ok(file) => {
                stats.symbols += file.facts.symbols.len();
                stats.succeeded += 1;
                items.push(file);
            }
            Err((_rel_path, _msg)) => {
                stats.failed += 1;
            }
        }

        if let Some(cb) = on_progress {
            cb(i + 1, total);
        }
    }

    ExtractedFiles { items, stats }
}

// ── Phase 5b: Extraction (parallel) ─────────────────────────────────────

/// Extract facts from files in parallel using rayon — no DB writes.
///
/// Shares a single [`ParseWorkerPool`] across all rayon threads.  Atomic
/// counters track per-file success/failure/symbol counts thread-safely.
/// `on_progress` is called once at the end with `(succeeded, total)`.
/// `on_file_progress` is called every 50 files with `(completed, total)`.
///
/// Delegates to [`phase_extract_parallel_cancellable`] with
/// `cancel_token: None`.
pub fn phase_extract_parallel(
    root: &Path,
    files: &[PathBuf],
    frontends: &HashMap<Language, LanguageFrontend>,
    mode: ExtractionMode,
    on_progress: Option<&dyn Fn(usize, usize)>,
    on_file_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> ExtractedFiles {
    phase_extract_parallel_cancellable(
        root,
        files,
        frontends,
        mode,
        on_progress,
        on_file_progress,
        None,
    )
}

/// Extract facts from files in parallel using rayon — cancellable variant.
///
/// Identical to [`phase_extract_parallel`] but supports a cancel token
/// (`AtomicBool`).  Before processing each file, the cancel token is
/// checked with [`Ordering::Relaxed`]; if `true` the file is skipped.
/// This allows external interrupt handlers (e.g. Ctrl-C) to stop extraction
/// early without waiting for the current batch to complete.
///
/// # Safety / ordering
///
/// The cancel token uses [`Ordering::Relaxed`] because:
/// - Rayon parallel iterators schedule work items on the same underlying
///   thread pool, not on separate threads that need happens-before edges.
/// - A false-negative (missing a just-set cancel) is harmless — the file
///   will be processed as normal and the next file will see the flag.
///
/// `on_file_progress` is throttled to every 50 files to avoid contention
/// on the shared [`AtomicUsize`] counter.
pub fn phase_extract_parallel_cancellable(
    root: &Path,
    files: &[PathBuf],
    frontends: &HashMap<Language, LanguageFrontend>,
    mode: ExtractionMode,
    on_progress: Option<&dyn Fn(usize, usize)>,
    on_file_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancel_token: Option<&std::sync::atomic::AtomicBool>,
) -> ExtractedFiles {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = ParseWorkerPool::new(WorkerConfig::default());
    let total = files.len();

    // Atomic counters for thread-safe progress
    let succeeded = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let symbol_count = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);

    let items: Vec<ExtractedFile> = files
        .par_iter()
        .filter_map(|rel_path| {
            // Check cancel token before processing this file
            if let Some(token) = cancel_token {
                if token.load(Ordering::Relaxed) {
                    return None;
                }
            }

            let abs_path = root.join(rel_path);

            let result = (|| -> Option<ExtractedFile> {
                let lang = Language::from_path(rel_path)?;
                let frontend = frontends.get(&lang)?;
                match extract_one_index_file(&pool, &abs_path, root, frontend, &mode) {
                    Ok(file) => {
                        symbol_count.fetch_add(file.facts.symbols.len(), Ordering::Relaxed);
                        succeeded.fetch_add(1, Ordering::Relaxed);
                        Some(file)
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })();

            // Per-file progress: throttled to every 50 items to avoid
            // AtomicUsize contention across rayon threads.
            let c = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(cb) = on_file_progress {
                if c % 50 == 0 || c == total {
                    cb(c, total);
                }
            }

            result
        })
        .collect();

    // Call on_progress once at the end if provided (backward-compat hook)
    if let Some(cb) = on_progress {
        cb(succeeded.load(Ordering::Relaxed), total);
    }

    ExtractedFiles {
        items,
        stats: ExtractionPhaseStats {
            attempted: total,
            succeeded: succeeded.load(Ordering::Relaxed),
            failed: failed.load(Ordering::Relaxed),
            symbols: symbol_count.load(Ordering::Relaxed),
        },
    }
}

/// Internal: extract a single file.  Returns `Err(rel_path, message)` on
/// failure so the caller can distinguish per-file errors from fatal errors.
fn extract_one_index_file(
    pool: &ParseWorkerPool,
    abs_path: &Path,
    project_root: &Path,
    frontend: &LanguageFrontend,
    mode: &ExtractionMode,
) -> std::result::Result<ExtractedFile, (PathBuf, String)> {
    let rel_path = abs_path
        .strip_prefix(project_root)
        .unwrap_or(abs_path)
        .to_path_buf();

    let source = std::fs::read_to_string(abs_path).map_err(|e| {
        (
            rel_path.clone(),
            format!("Read failed for {}: {:#}", abs_path.display(), e),
        )
    })?;

    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let file_id = source_file_id(&rel_path).map_err(|e| {
        (
            rel_path.clone(),
            format!("Invalid source path {}: {:#}", rel_path.display(), e),
        )
    })?;

    let _lang = Language::from_path(&rel_path)
        .ok_or_else(|| (rel_path.clone(), "No language detected".to_string()))?;

    let facts = pool
        .extract_one(
            frontend,
            file_id,
            &rel_path,
            &source,
            &content_hash,
            mode.clone(),
        )
        .map_err(|e| {
            (
                rel_path.clone(),
                format!("Extraction failed: {}", e.message),
            )
        })?;

    Ok(ExtractedFile {
        rel_path,
        language: facts.file.language,
        facts,
    })
}

// ── Phase 6: DB write ──────────────────────────────────────────────────

/// Write facts one-at-a-time (MCP / sync-engine path).
///
/// Returns the number of files successfully inserted.
pub fn phase_write_single(store: &Arc<Store>, extracted: &ExtractedFiles) -> Result<usize> {
    let mut written = 0;
    for file in &extracted.items {
        store
            .insert_file_facts(&file.facts)
            .with_context(|| format!("Failed to insert facts for {}", file.rel_path.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Batch-write facts with WAL checkpoint (CLI path).
///
/// Uses bulk-write mode (synchronous = OFF, foreign_keys = OFF) for
/// throughput, with PASSIVE WAL checkpoints to keep the WAL bounded.
///
/// - `on_progress(written: u64)` — called after each chunk is committed.
/// - `interrupted() -> bool` — if true, stops early and returns whatever
///   was written so far.
pub fn phase_write_batched(
    store: &Arc<Store>,
    extracted: &ExtractedFiles,
    batch_size: usize,
    checkpoint_interval: u64,
    mut on_progress: impl FnMut(u64),
    mut interrupted: impl FnMut() -> bool,
) -> Result<WriteBatchStats> {
    anyhow::ensure!(batch_size > 0, "batch_size must be > 0, got {batch_size}");

    let mut stats = WriteBatchStats {
        written: 0,
        batch_failures: 0,
        single_failures: 0,
    };

    let _bulk = store.enter_bulk_write()?;
    let mut next_checkpoint = checkpoint_interval;

    for chunk in extracted.items.chunks(batch_size) {
        if interrupted() {
            return Ok(stats);
        }
        let facts: Vec<_> = chunk.iter().map(|ef| ef.facts.clone()).collect();
        if store.insert_file_facts_batch(&facts).is_err() {
            stats.batch_failures += 1;
            for ef in chunk {
                match store.insert_file_facts(&ef.facts) {
                    Ok(_) => {
                        stats.written += 1;
                    }
                    Err(_) => {
                        stats.single_failures += 1;
                    }
                }
            }
        } else {
            stats.written += chunk.len();
        }

        if stats.written as u64 >= next_checkpoint {
            let _ = store.checkpoint_wal();
            next_checkpoint = stats.written as u64 + checkpoint_interval;
        }

        on_progress(stats.written as u64);
    }

    let _ = store.checkpoint_wal_truncate();
    Ok(stats)
}

// ── Phase 7: Resolution + edge building ────────────────────────────────

/// Resolve symbol references and build graph edges.
///
/// Checks whether path alias config (`tsconfig.json` / `jsconfig.json`) has
/// changed since the last index; if so, invalidates all resolved references
/// and deletes all existing edges before re-resolving.  Resolution itself
/// runs in parallel via [`ReferenceResolver::resolve_all_parallel`].
pub fn phase_resolve_and_build(store: &Arc<Store>, root: &Path) -> Result<GraphResult> {
    let path_alias = PathAliasConfig::resolver(root);
    if PathAliasConfig::has_changed(store, root)? {
        store.invalidate_all_references()?;
        store.delete_all_edges()?;
    }
    let mut resolver = ReferenceResolver::with_path_alias(store.clone(), path_alias);
    let (resolved_refs, res_stats) = resolver
        .resolve_all_parallel(store.clone(), None, None)
        .context("Reference resolution failed")?;
    let builder = GraphBuilder::new(store.clone());
    let build_stats = builder.build_all(&resolved_refs);
    Ok(GraphResult {
        resolved: res_stats.resolved,
        edges_built: build_stats.edges_built,
    })
}

/// Materialize user-declared function-pointer annotations as graph edges.
pub fn phase_materialize_annotations(store: &Arc<Store>) -> Result<()> {
    graph::materialize_annotations(store)
        .map(|_| ())
        .context("Failed to materialize annotations")
}

/// Build persistent function summaries (Schema v3 / analysis surface).
///
/// Returns the number of functions summarised.
pub fn phase_build_summaries(store: &Arc<Store>) -> Result<usize> {
    let stats = db::summary::SummaryStore::build_all(store, |s, fid| {
        analysis::summary::SummaryBuilder::build(s, fid, None)
    })
    .context("Failed to build summaries")?;

    // Record "summaries" layer in extraction_state so get_capability_mask()
    // returns the SUMMARIES bit for files that have function summaries.
    if stats.functions_summarized > 0 {
        record_summaries_extraction_state(store)?;
    }

    Ok(stats.functions_summarized)
}

/// Write extraction_state rows for the "summaries" layer so capability
/// queries can detect that inter-procedural summaries are available.
///
/// Only records files that are still content-fresh (content_hash matches
/// `files`), since stale summaries are not trustworthy.
fn record_summaries_extraction_state(store: &Arc<Store>) -> Result<()> {
    use types::structs::CapabilityMask;

    let files = db::summary::SummaryStore::files_with_summaries(store)?;
    for (file_id, content_hash) in &files {
        store.upsert_file_extraction_state(
            file_id,
            "summaries",
            content_hash,
            "complete",
            CapabilityMask::new(CapabilityMask::SUMMARIES),
        )?;
    }
    Ok(())
}

/// Commit the current path-alias config hash baseline.
///
/// Call this **after** invalidation (when path alias config has changed)
/// to record the new baseline.  The detect→invalidate→commit ordering
/// is critical — committing before invalidation would cause the next
/// `PathAliasConfig::has_changed()` to return `false` while stale
/// references/edges remain.
pub fn phase_commit_path_alias_config(store: &Arc<Store>, root: &Path) -> Result<()> {
    PathAliasConfig::commit(store, root)
}

/// Finalize the index: write metadata (last_index_time, last_index_root,
/// indexed_scope).  Does **not** commit path alias config — use
/// [`phase_commit_path_alias_config`] for that.
pub fn phase_finalize(store: &Arc<Store>, root: &Path, scope_patterns: &[String]) -> Result<()> {
    store.set_metadata(
        "last_index_time",
        &std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string(),
    )?;
    store.set_metadata("last_index_root", &root.display().to_string())?;
    let scope_json = if scope_patterns.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(scope_patterns).unwrap_or_else(|_| "[]".to_string())
    };
    store.set_metadata("indexed_scope", &scope_json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_discover_finds_ts_in_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.ts"), "const x = 1;\n").unwrap();
        std::fs::write(dir.path().join("utils.ts"), "export const y = 2;\n").unwrap();

        let files = phase_discover(dir.path(), &[], &[]).unwrap();
        assert_eq!(files.len(), 2);
        // Both files should exist (order not guaranteed)
        let names: Vec<&str> = files.iter().map(|p| p.to_str().unwrap()).collect();
        assert!(names.contains(&"main.ts"));
        assert!(names.contains(&"utils.ts"));
    }

    #[test]
    fn phase_extract_serial_extracts_ts_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hello.ts"),
            "export function greet(name: string) { return `hi ${name}`; }\n",
        )
        .unwrap();

        let files = vec![PathBuf::from("hello.ts")];
        let frontends = phase_init_frontends(&files).unwrap();
        let result = phase_extract_serial(
            dir.path(),
            &files,
            &frontends,
            ExtractionMode::Manifest,
            None,
        );

        assert_eq!(result.stats.attempted, 1);
        assert_eq!(result.stats.succeeded, 1);
        assert_eq!(result.stats.failed, 0);
        assert!(result.stats.symbols > 0);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].language, Language::TypeScript);
        assert_eq!(result.items[0].rel_path, PathBuf::from("hello.ts"));
    }

    #[test]
    fn phase_write_single_persists_facts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("calc.ts"),
            "export function add(a: number, b: number): number { return a + b; }\n",
        )
        .unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let files = vec![PathBuf::from("calc.ts")];
        let frontends = phase_init_frontends(&files).unwrap();
        let extracted = phase_extract_serial(
            dir.path(),
            &files,
            &frontends,
            ExtractionMode::Manifest,
            None,
        );

        let written = phase_write_single(&store, &extracted).unwrap();
        assert_eq!(written, 1);
        assert!(store.count_symbols().unwrap() > 0);
    }

    /// Verify that `phase_write_batched` holds the `BulkWriteGuard` for the
    /// entire insert loop, then restores pragmas on return.
    #[test]
    fn phase_write_batched_guard_covers_inserts_and_restores_pragmas() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.ts"),
            "export function fa() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.ts"),
            "export function fb() { return 2; }\n",
        )
        .unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let files = vec![PathBuf::from("a.ts"), PathBuf::from("b.ts")];
        let frontends = phase_init_frontends(&files).unwrap();
        let extracted = phase_extract_serial(
            dir.path(),
            &files,
            &frontends,
            ExtractionMode::Manifest,
            None,
        );
        assert_eq!(
            extracted.items.len(),
            2,
            "expected both files to be extracted"
        );

        // Run batched write with a small batch size so both files land in
        // the batch path.
        let stats = phase_write_batched(
            &store,
            &extracted,
            2,        // batch_size
            100,      // checkpoint_interval
            |_| {},   // on_progress (no-op)
            || false, // interrupted (never)
        )
        .unwrap();

        assert_eq!(stats.written, 2, "both files should be written");
        assert_eq!(stats.batch_failures, 0);
        assert_eq!(stats.single_failures, 0);

        // Verify data actually landed.
        let count = store.count_symbols().unwrap();
        assert!(count > 0, "symbol count was {count}");

        // Verify pragmas were restored after the guard dropped.
        // For in-memory databases the BulkWriteGuard sets synchronous=NORMAL(1)
        // and foreign_keys=ON(1) on drop.
        let sync_val: i32 = store
            .with_transaction(|tx| Ok(tx.query_row("PRAGMA synchronous", [], |row| row.get(0))?))
            .expect("query PRAGMA synchronous");
        assert_eq!(
            sync_val, 1,
            "synchronous should be NORMAL (1) after bulk-write guard drops, got {}",
            sync_val
        );

        let fk_val: i32 = store
            .with_transaction(|tx| Ok(tx.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?))
            .expect("query PRAGMA foreign_keys");
        assert_eq!(
            fk_val, 1,
            "foreign_keys should be ON (1) after bulk-write guard drops, got {}",
            fk_val
        );
    }

    /// `phase_extract_parallel_cancellable` calls `on_file_progress` at least
    /// once per 50 files (and once at the end for the total).  Writing >50
    /// TypeScript files exercises the throttle logic and verifies the callback
    /// fires.
    #[test]
    fn phase_extract_parallel_calls_on_file_progress() {
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();

        // Create 60 TypeScript files — enough to cross the 50-file throttle
        // boundary at least once.
        let mut paths = Vec::new();
        for i in 0..60 {
            let name = format!("file_{:03}.ts", i);
            std::fs::write(
                dir.path().join(&name),
                format!("export const x_{i} = {i};\n"),
            )
            .unwrap();
            paths.push(PathBuf::from(name));
        }

        let frontends = phase_init_frontends(&paths).unwrap();
        let calls: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

        let result = phase_extract_parallel_cancellable(
            dir.path(),
            &paths,
            &frontends,
            ExtractionMode::Manifest,
            None, // on_progress — unused
            Some(&|completed, total| {
                calls.lock().unwrap().push((completed, total));
            }),
            None, // cancel_token — not used
        );

        assert_eq!(result.stats.succeeded, 60, "all 60 files should succeed");
        assert_eq!(result.stats.failed, 0);

        let records = calls.lock().unwrap();
        // At minimum we expect: one call at 50 and one at 60.
        assert!(
            records.len() >= 2,
            "expected at least 2 on_file_progress calls, got {}",
            records.len()
        );
        // The last call should report total=60.
        let last = records.last().unwrap();
        assert_eq!(last.1, 60, "last progress call should report total=60");
    }

    /// `phase_extract_parallel_cancellable` skips files when the cancel token
    /// is set, resulting in fewer extracted items than the input length.
    #[test]
    fn phase_extract_parallel_respects_cancel() {
        let dir = tempfile::tempdir().unwrap();

        let mut paths = Vec::new();
        for i in 0..20 {
            let name = format!("file_{:03}.ts", i);
            std::fs::write(
                dir.path().join(&name),
                format!("export const x_{i} = {i};\n"),
            )
            .unwrap();
            paths.push(PathBuf::from(name));
        }

        let frontends = phase_init_frontends(&paths).unwrap();

        // Set the cancel token from the start — every file should be skipped.
        let cancel = std::sync::atomic::AtomicBool::new(true);

        let result = phase_extract_parallel_cancellable(
            dir.path(),
            &paths,
            &frontends,
            ExtractionMode::Manifest,
            None,
            None, // on_file_progress — unused
            Some(&cancel),
        );

        assert_eq!(
            result.items.len(),
            0,
            "no files should be extracted when cancel is true from the start"
        );
        assert_eq!(result.stats.succeeded, 0);
        assert_eq!(result.stats.failed, 0);
        assert_eq!(result.stats.attempted, 20);
    }
}
