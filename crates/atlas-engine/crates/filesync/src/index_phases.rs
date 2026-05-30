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
    anyhow::ensure!(batch_size > 0, "batch_size must be > 0, got {}", batch_size);

    let mut stats = WriteBatchStats {
        written: 0,
        batch_failures: 0,
        single_failures: 0,
    };

    store.begin_bulk_write()?;
    let mut next_checkpoint = checkpoint_interval;

    for chunk in extracted.items.chunks(batch_size) {
        if interrupted() {
            store.end_bulk_write()?;
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

    store.end_bulk_write()?;
    let _ = store.checkpoint_wal_truncate();
    Ok(stats)
}

// ── Phase 7: Resolution + edge building ────────────────────────────────

/// Resolve symbol references and build graph edges.
pub fn phase_resolve_and_build(store: &Arc<Store>, root: &Path) -> Result<GraphResult> {
    let path_alias = PathAliasConfig::resolver(root);
    let mut resolver = ReferenceResolver::with_path_alias(store.clone(), path_alias);
    let (resolved_refs, res_stats) = resolver
        .resolve_all()
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
    Ok(stats.functions_summarized)
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
}
