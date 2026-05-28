//! `atlas index` command — walk project tree (git-aware), extract facts, resolve references.
//!
//! ## Design
//! - **Worker thread**: runs the 9-phase indexing pipeline, updates ProgressState.
//! - **Main thread**: runs the terminal progress loop (or text fallback).
//! - **Phase 1 (parallel)**: Extract all files using Rayon — CPU-bound, no SQLite access.
//! - **Phase 2 (sequential)**: Insert extracted facts into the store — SQLite single-writer.
//! - **Phase 3**: Resolve all references (parallel matching + serial write).
//!
//! ## P6: Progress reporting
//! Every phase reports progress via `ProgressState`:
//! - Parallel phases (Extraction, Resolution Phase 1, EdgeBuilding): AtomicU64 counters.
//! - Serial phases (HashCheck, DB Write, Resolution Phase 2): direct `set_current()`.

use crate::tui::{TextFallback, TuiProgress};
use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_engine::ExtractionError;
use atlas_engine::ExtractionMode;
use atlas_engine::FailureCategory;
use atlas_engine::FileLock;
use atlas_engine::Language;
use atlas_engine::LanguageFrontend;
use atlas_engine::SourcePath;
use atlas_engine::discovery::{DiscoveryConfig, discover_files};
use atlas_engine::progress::{ProgressPhase, ProgressState};
use atlas_engine::{self, LanguageRegistry, ParseWorkerPool, WorkerConfig};
use atlas_engine::PerLanguageStats;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Result of extracting a single file.
struct ExtractedFile {
    _rel_path: PathBuf,
    lang: Language,
    facts: atlas_engine::FileFacts,
}

/// Result of the hash-check phase.
struct HashCheckResult {
    dirty: Vec<PathBuf>,
    clean_count: usize,
    deleted: Vec<PathBuf>,
}

pub fn run(
    project: &str,
    includes: &[String],
    scopes: &[String],
    exclude: &[String],
    analysis: &str,
) -> anyhow::Result<()> {
    let mode = match analysis {
        "manifest" => ExtractionMode::Manifest,
        "full" => ExtractionMode::Full,
        _ => ExtractionMode::Structural,
    };

    // ── Configure rayon thread pool (once, idempotent) ──────────────────
    // macOS spawns threads with a 512 KB stack by default, which overflows
    // during tree-sitter's recursive-descent parsing of deeply-nested files
    // (e.g. Linux kernel headers).  4 MB matches the tree-sitter playground
    // convention and is safe on all platforms.
    //
    // `build_global()` can only succeed once per process; subsequent calls
    // (e.g. in integration tests that run `index::run()` multiple times) will
    // error.  Using `Once` ensures we only attempt the first time.
    static RAYON_INIT: std::sync::Once = std::sync::Once::new();
    RAYON_INIT.call_once(|| {
        rayon::ThreadPoolBuilder::new()
            .stack_size(4 * 1024 * 1024)
            .build_global()
            .expect("failed to initialise rayon thread pool");
    });

    // ── Merge include/scope patterns ──
    let mut include_patterns: Vec<String> = includes.to_vec();
    for scope in scopes {
        include_patterns.push(scope_to_glob(scope));
    }

    // ── Open store (before spawning worker, in case of immediate error) ──
    let ctx = CommandContext::open(project, DbMode::CreateOrOpenReadWrite)?;
    let _lock = FileLock::acquire(&ctx.store)
        .context("Another atlas process is indexing this project.")?;

    // ── Shared state ──
    let progress_state = Arc::new(Mutex::new(ProgressState::new()));
    let done_flag = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::new(AtomicBool::new(false));

    // ── Ctrl+C handler ──
    // First press: graceful shutdown (stop_flag → main thread exits draw loop).
    // Second press: immediate exit (terminal may be stuck; OS-level kill).
    let stop = stop_flag.clone();
    let press_count = Arc::new(AtomicU64::new(0));
    let pc = press_count.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        let n = pc.fetch_add(1, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        if n >= 1 {
            // Second press — exit immediately.
            std::process::exit(1);
        }
    }) {
        eprintln!("warning: could not install Ctrl+C handler: {}", e);
    }

    // ── Start TUI (or text fallback if non-TTY) ──
    let mut tui = TuiProgress::try_init(progress_state.clone());
    let has_tty = tui.is_some();

    // ── Clone state for worker ──
    let ps_worker = progress_state.clone();
    let done_w = done_flag.clone();
    let stop_w = stop_flag.clone();
    let root_path = ctx.root.as_path().to_path_buf();
    let store_arc = ctx.store.clone();
    let store_for_main = store_arc.clone();
    let include_clone = include_patterns.clone();
    let exclude_clone = exclude.to_vec();

    // ── Spawn worker thread ──
    let worker = std::thread::spawn(move || -> anyhow::Result<()> {
        let root = root_path;
        let store = store_arc;
        let include_patterns = include_clone;
        let exclude = exclude_clone;
        let ps = ps_worker;

        // Helper: check stop flag
        let interrupted = || stop_w.load(Ordering::SeqCst);

        // ── Discovery ──
        ps.lock().unwrap().start_phase(ProgressPhase::Discovery, None);
        let mut config = DiscoveryConfig::default();
        if !include_patterns.is_empty() {
            config.include_patterns = include_patterns.clone();
        }
        if !exclude.is_empty() {
            config.exclude_patterns = exclude.clone();
        }
        let discovered = discover_files(&root, &config).context("Failed to discover files")?;
        if discovered.is_empty() {
            ps.lock().unwrap().start_phase(ProgressPhase::Finalizing, None);
            anyhow::bail!("No recognizable source files found in {}", root.display());
        }
        let total = discovered.len();
        ps.lock().unwrap().start_phase(
            ProgressPhase::HashCheck,
            Some(format!("{} files", total)),
        );

        if interrupted() { return Ok(()); }

        // ── Hash check ──
        let hash_result = build_dirty_set(&store, &discovered, &root)?;
        let dirty = &hash_result.dirty;
        let reused = hash_result.clean_count;
        ps.lock().unwrap().start_phase(
            ProgressPhase::Cleanup,
            Some(format!("{} dirty / {} reused", dirty.len(), reused)),
        );

        if interrupted() { return Ok(()); }

        // ── Delete stale data ──
        if !hash_result.deleted.is_empty() {
            let deleted_count = hash_result.deleted.len() as u64;
            ps.lock().unwrap().set_total(deleted_count);
            let mut i = 0u64;
            for rel_path in &hash_result.deleted {
                let sp = SourcePath::try_from_relative(&rel_path.to_string_lossy())
                    .context("invalid deleted path")?;
                let file_id = atlas_engine::FileId::generate(sp.as_str());
                store.invalidate_references_to_symbols_in_file(&file_id)?;
                store.delete_edges_for_file_references(&file_id)?;
                store.delete_file_data(&file_id)?;
                i += 1;
                if i % 50 == 0 {
                    ps.lock().unwrap().set_current(i);
                }
            }
            ps.lock().unwrap().set_current(deleted_count);
        }

        let languages: Vec<Language> = dirty.iter()
            .filter_map(|p| Language::from_path(p))
            .fold(Vec::new(), |mut acc, lang| {
                if !acc.contains(&lang) { acc.push(lang); }
                acc
            });

        if interrupted() { return Ok(()); }

        // ── Language init ──
        ps.lock().unwrap().start_phase(
            ProgressPhase::LanguageInit,
            Some(format!("{} languages", languages.len())),
        );
        let _registry = LanguageRegistry::new(&languages)
            .or_else(|e| {
                let available: Vec<Language> = languages.iter()
                    .filter(|l| LanguageRegistry::new(&[**l]).is_ok())
                    .copied()
                    .collect();
                if available.is_empty() {
                    Err(e)
                } else {
                    LanguageRegistry::new(&available)
                }
            })?;
        let frontend_cache: HashMap<Language, LanguageFrontend> = languages.iter()
            .filter_map(|&lang| atlas_engine::create_frontend(lang).map(|fe| (lang, fe)))
            .collect();

        if interrupted() { return Ok(()); }

        // ── Extraction (parallel) ──
        let dirty_total = dirty.len();
        ps.lock().unwrap().start_phase(
            ProgressPhase::Extraction,
            Some(format!("{} files", dirty_total)),
        );
        ps.lock().unwrap().set_total(dirty_total as u64);

        let pool = ParseWorkerPool::new(WorkerConfig::default());
        let extracted_count = AtomicUsize::new(0);
        let per_lang_mutex = Mutex::new(PerLanguageStats::new());
        let fc = &frontend_cache;
        let extract_counter = ps.lock().unwrap().atomic_current.clone();
        let count_atomic = &extracted_count;

        let results: Vec<_> = dirty
            .par_iter()
            .filter_map(|rel_path| {
                if interrupted() { return None; }
                let abs_path = root.join(rel_path);
                let lang = Language::from_path(rel_path)?;
                let frontend = fc.get(&lang)?;
                let file_start = Instant::now();
                let result = extract_one_with_frontend(
                    &pool, &abs_path, &root, lang, frontend, mode.clone(),
                );
                let extract_ms = file_start.elapsed().as_millis() as u64;

                let _count = count_atomic.fetch_add(1, Ordering::Relaxed);
                extract_counter.fetch_add(1, Ordering::Relaxed);

                let (facts_opt, failed, fail_cat) = match result {
                    Ok(facts) => (Some(facts), false, None),
                    Err(ref _e) => (None, true, Some("extraction_error")),
                };
                {
                    per_lang_mutex.lock().unwrap_or_else(|e| e.into_inner())
                        .record_file(lang, extract_ms, failed, fail_cat);
                }
                facts_opt.map(|facts| ExtractedFile { _rel_path: rel_path.clone(), lang, facts })
            })
            .collect();

        if interrupted() { return Ok(()); }

        let extracted = results;
        let extracted_count = extracted.len();
        let _failed_count = dirty_total.saturating_sub(extracted_count);

        let mut per_lang = per_lang_mutex.into_inner().unwrap();

        // ── Clean stale facts ──
        ps.lock().unwrap().start_phase(
            ProgressPhase::Cleanup,
            Some(format!("{} re-indexed", extracted_count)),
        );
        let file_ids: Vec<_> = extracted.iter().map(|ef| ef.facts.file.file_id).collect();
        for fid in &file_ids {
            let _ = store.invalidate_references_to_symbols_in_file(fid);
        }
        store.delete_files_batch(&file_ids)
            .context("Failed to clean stale facts")?;

        if interrupted() { return Ok(()); }

        // ── DB Write (serial, with progress) ──
        ps.lock().unwrap().start_phase(
            ProgressPhase::DbWrite,
            Some(format!("{} files", extracted_count)),
        );
        ps.lock().unwrap().set_total(extracted_count as u64);

        // Bulk-write mode: disable synchronous & FK checks.  Keep
        // wal_autocheckpoint at default (1000 pages) so the WAL
        // self-truncates — for full-analysis, each batch easily
        // produces > 1000 pages of WAL, and a growing WAL makes
        // subsequent transactions O(WAL-size) slower.
        store.begin_bulk_write()?;

        let mut _insert_failures = 0usize;
        // 100 files/txn — full-analysis data is dense (thousands of
        // rows per file for dataflow/CFG/bindings).  Larger batches
        // choke SQLite's B-tree with millions of rows per transaction.
        const BATCH_SIZE: usize = 100;
        let mut written = 0u64;
        // PASSIVE WAL checkpoint every 500 files to keep the WAL
        // below ~200 MB even under full-analysis load.
        const CHECKPOINT_INTERVAL: u64 = 500;
        let mut next_checkpoint = CHECKPOINT_INTERVAL;
        for chunk in extracted.chunks(BATCH_SIZE) {
            if interrupted() {
                store.end_bulk_write()?;
                return Ok(());
            }
            let facts: Vec<_> = chunk.iter().map(|ef| ef.facts.clone()).collect();
            if let Err(_e) = store.insert_file_facts_batch(&facts) {
                for ef in chunk {
                    match store.insert_file_facts(&ef.facts) {
                        Ok(_) => {}
                        Err(_) => {
                            _insert_failures += 1;
                            per_lang.record_file(ef.lang, 0, true, Some("db_insert_error"));
                        }
                    }
                }
            }
            written += chunk.len() as u64;
            if written >= next_checkpoint {
                let _ = store.checkpoint_wal();
                next_checkpoint = written + CHECKPOINT_INTERVAL;
            }
            ps.lock().unwrap().set_current(written);
        }

        // Restore safety defaults and flush WAL.
        store.end_bulk_write()?;
        let _ = store.checkpoint_wal_truncate();

        if interrupted() { return Ok(()); }

        // ── Resolution (parallel matching + serial write) ──
        let path_alias = atlas_engine::PathAliasResolver::from_tsconfig(&root.join("tsconfig.json"))
            .or_else(|| atlas_engine::PathAliasResolver::from_jsconfig(&root.join("jsconfig.json")))
            .unwrap_or_else(atlas_engine::PathAliasResolver::empty);

        let tsconfig_changed = atlas_engine::detect_config_change(
            &store, &root, &["tsconfig.json", "jsconfig.json"],
        )?;
        if tsconfig_changed {
            store.invalidate_all_references()?;
            store.delete_all_edges()?;
        }

        let mut resolver = atlas_engine::ReferenceResolver::with_path_alias(
            store.clone(), path_alias,
        );
        let (resolved, _stats) = resolver.resolve_all_parallel(
            store.clone(),
            Some(&ps),
            None,
        )?;

        if interrupted() { return Ok(()); }

        // ── Edge building (already par_iter internally) ──
        ps.lock().unwrap().start_phase(ProgressPhase::EdgeBuilding, None);
        let builder = atlas_engine::GraphBuilder::new(store.clone());
        let _build_stats = builder.build_all(&resolved);
        ps.lock().unwrap().set_current(resolved.len() as u64);

        // ── Materialize user annotations as edges ──
        if let Err(e) = atlas_engine::materialize_annotations(&store) {
            eprintln!("Warning: failed to materialize annotations: {}", e);
        }

        // ── Summary build (Schema v3: persist function summaries) ──
        ps.lock().unwrap().start_phase(
            ProgressPhase::Finalizing,
            Some("Building summaries...".into()),
        );
        let _summary_stats = atlas_engine::SummaryStore::build_all(&store, |s, fid| {
            atlas_engine::SummaryBuilder::build(s, fid, None)
        })?;
        if interrupted() { return Ok(()); }

        // ── Finalize ──
        if tsconfig_changed {
            atlas_engine::commit_config_hashes(&store, &root, &["tsconfig.json"])?;
        }
        store.set_metadata(
            "last_index_time",
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string(),
        )?;
        store.set_metadata("last_index_root", &root.display().to_string())?;
        store.set_metadata(
            "indexed_scope",
            &indexed_scope_json(&include_patterns),
        )?;

        ps.lock().unwrap().start_phase(ProgressPhase::Finalizing, None);

        done_w.store(true, Ordering::SeqCst);
        Ok(())
    });

    // ── Main thread: render loop ──
    let was_interrupted = if has_tty {
        tui.as_mut().unwrap().draw_loop(&done_flag, &stop_flag)
    } else {
        // Text fallback loop
        let mut fb = TextFallback::new(progress_state.clone());
        loop {
            fb.tick();
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            if done_flag.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        stop_flag.load(Ordering::SeqCst) // was_interrupted = stop_flag is set
    };

    // ── Join worker unconditionally ──
    // The worker checks stop_flag at phase boundaries and will exit cleanly.
    // We MUST join before returning so the FileLock RAII guard (line 89)
    // isn't dropped while the worker is still accessing the database.
    if was_interrupted {
        tracing::info!("interrupted: waiting for worker to finish...");
    }
    let worker_result = worker.join().unwrap_or_else(|e| {
        Err(anyhow::anyhow!("Worker thread panicked: {:?}", e))
    });

    // Report worker result (whether it completed or was interrupted).
    if let Err(ref e) = worker_result {
        if was_interrupted {
            tracing::warn!("worker thread returned error during interrupt: {:?}", e);
        }
    }

    if was_interrupted {
        if has_tty {
            tui.take()
                .unwrap()
                .interrupt(&progress_state.lock().unwrap());
        } else {
            eprintln!();
            crate::tui::progress::print_interrupted(&progress_state.lock().unwrap());
        }
        return Ok(());
    }

    // Normal completion: propagate worker errors.
    worker_result?;

    let db_stats = store_for_main.get_stats()?;

    if has_tty {
        // Restore the inline TUI, then print the final summary as normal
        // command output so the shell prompt follows it immediately.
        tui.take().unwrap().finish(
            db_stats.total_files as u64,
            db_stats.total_symbols as u64,
            db_stats.total_edges as u64,
        );
    } else {
        // Text fallback (non-TTY): print summary to stdout.
        println!("Database status:");
        println!("  Files:    {}", db_stats.total_files);
        println!("  Symbols:  {}", db_stats.total_symbols);
        println!("  Edges:    {}", db_stats.total_edges);
        println!("\nIndex complete.");
    }

    Ok(())
}

// ── P1: Hash-based dirty set computation ──────────────────────────────────

fn build_dirty_set(
    store: &atlas_engine::Store,
    discovered: &[PathBuf],
    root: &Path,
) -> anyhow::Result<HashCheckResult> {
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

    let db_files = store.list_files().unwrap_or_default();
    let db_hashes: HashMap<String, String> = db_files
        .iter()
        .map(|f| (f.path.clone(), f.content_hash.clone()))
        .collect();
    let db_paths: HashSet<String> = db_hashes.keys().cloned().collect();

    let mut dirty = Vec::new();
    let mut clean_count = 0usize;
    let discovered_set: HashSet<String> = current_hashes.keys().cloned().collect();

    for rel_path in discovered {
        let key = match SourcePath::try_from_relative(&rel_path.to_string_lossy()) {
            Ok(sp) => sp.as_str().to_string(),
            Err(_) => continue,
        };
        match db_hashes.get(&key) {
            None => { dirty.push(rel_path.clone()); }
            Some(db_hash) => {
                if let Some(curr_hash) = current_hashes.get(&key) {
                    if curr_hash == db_hash { clean_count += 1; }
                    else { dirty.push(rel_path.clone()); }
                } else { dirty.push(rel_path.clone()); }
            }
        }
    }

    let deleted: Vec<PathBuf> = db_paths.difference(&discovered_set)
        .map(|p| PathBuf::from(p))
        .collect();

    Ok(HashCheckResult { dirty, clean_count, deleted })
}

// ── Extraction helpers ────────────────────────────────────────────────────

fn extract_one_with_frontend(
    pool: &ParseWorkerPool,
    path: &Path,
    root: &Path,
    _lang: Language,
    frontend: &LanguageFrontend,
    mode: atlas_engine::ExtractionMode,
) -> Result<atlas_engine::FileFacts, ExtractionError> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let relative = path.strip_prefix(root).unwrap_or(path);
            let rel_str = relative.to_string_lossy().to_string();
            let msg = format!("Failed to read {}: {}", path.display(), e);
            pool.push_failure(&rel_str, FailureCategory::IoError, msg.clone());
            return Err(ExtractionError {
                file_path: rel_str, category: FailureCategory::IoError, message: msg,
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
    pool.extract_one(frontend, file_id, relative, &source, &content_hash, mode)
}

// ── Scope helpers ──────────────────────────────────────────────────────────

fn scope_to_glob(scope: &str) -> String {
    if scope.contains('*') { scope.to_string() }
    else { format!("{}/**", scope.trim_end_matches('/')) }
}

fn indexed_scope_json(patterns: &[String]) -> String {
    if patterns.is_empty() { "[]".to_string() }
    else { serde_json::to_string(patterns).unwrap_or_else(|_| "[]".to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_to_glob_bare_dir() {
        assert_eq!(scope_to_glob("drivers/net"), "drivers/net/**");
    }
    #[test]
    fn scope_to_glob_trailing_slash() {
        assert_eq!(scope_to_glob("net/"), "net/**");
    }
    #[test]
    fn scope_to_glob_already_glob() {
        assert_eq!(scope_to_glob("src/**/*.rs"), "src/**/*.rs");
    }
    #[test]
    fn indexed_scope_json_empty() {
        assert_eq!(indexed_scope_json(&[]), "[]");
    }
}
