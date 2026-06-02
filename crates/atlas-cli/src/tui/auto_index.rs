//! Auto-index: runs a manifest-mode index pipeline in a background thread
//! on TUI startup when the database is empty.
//!
//! ## Design
//! - **Worker thread**: runs the 7-phase manifest pipeline and updates [`AutoIndexProgress`].
//! - **Main thread**: polls `progress` every tick and renders a progress screen.
//! - **Pipeline**: Discovery → HashCheck → Cleanup → LanguageInit → Extraction → DbWrite → Finalize.
//! - **Skipped**: Resolution, EdgeBuilding, Summaries (manifest mode doesn't need them).
//! - **Not interruptible** (matches architecture doc).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::Context;
use atlas_engine::{
    ExtractedFile, ExtractedFiles, ExtractionMode, ExtractionPhaseStats, FileLock, Language,
    LanguageFrontend, LanguageRegistry, ParseWorkerPool, PerLanguageStats, Store, WorkerConfig,
};
use rayon::prelude::*;

// ── Progress ────────────────────────────────────────────────────────────────

/// Progress visible to the main TUI thread.
///
/// Updated by the worker thread under `Arc<Mutex<..>>`; polled by the
/// render loop every tick.
#[derive(Debug, Clone)]
pub struct AutoIndexProgress {
    /// Current phase name (e.g. "Discovery", "Extraction", "DbWrite").
    pub phase: String,
    /// Items processed so far in the current phase.
    pub current: u64,
    /// Total items in the current phase (0 if unknown).
    pub total: u64,
    /// Human-readable phase detail (e.g. "42 files").
    pub message: String,
}

// ── Handle ──────────────────────────────────────────────────────────────────

/// Handle returned by [`spawn_auto_index`].
///
/// The caller polls `progress` for UI updates and checks `done` to know
/// when the worker has finished.  After `done` is `true`, call
/// [`AutoIndexHandle::take_result`] to retrieve the outcome and join the
/// background thread.
pub struct AutoIndexHandle {
    /// Shared progress state — read from the TUI render loop.
    pub progress: Arc<Mutex<AutoIndexProgress>>,
    /// Set to `true` when the worker thread exits (success or error).
    pub done: Arc<AtomicBool>,
    /// Result of the pipeline — populated before `done` is set.
    pub result: Arc<Mutex<Option<anyhow::Result<()>>>>,
    handle: Option<JoinHandle<()>>,
}

impl AutoIndexHandle {
    /// Check if the worker thread completed successfully.
    ///
    /// Joins the handle and returns the stored result.
    /// Must only be called after `done` is `true`.
    pub fn take_result(&mut self) -> Option<anyhow::Result<()>> {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.result.lock().unwrap().take()
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Spawn a background thread that runs the manifest index pipeline.
///
/// Does **not** block — the caller polls [`AutoIndexHandle::progress`] for
/// UI updates and checks [`AutoIndexHandle::done`] for completion.
pub fn spawn_auto_index(store: Arc<Store>, project_root: PathBuf) -> AutoIndexHandle {
    let progress = Arc::new(Mutex::new(AutoIndexProgress {
        phase: "Initializing".into(),
        current: 0,
        total: 0,
        message: String::new(),
    }));
    let done = Arc::new(AtomicBool::new(false));
    let result: Arc<Mutex<Option<anyhow::Result<()>>>> = Arc::new(Mutex::new(None));

    let progress_w = Arc::clone(&progress);
    let done_w = Arc::clone(&done);
    let result_w = Arc::clone(&result);

    // Configure rayon thread pool (idempotent — same as CLI index).
    static RAYON_INIT: std::sync::Once = std::sync::Once::new();
    RAYON_INIT.call_once(|| {
        rayon::ThreadPoolBuilder::new()
            .stack_size(4 * 1024 * 1024)
            .build_global()
            .expect("failed to initialise rayon thread pool");
    });

    let handle = std::thread::spawn(move || {
        let outcome = run_manifest_pipeline(&store, &project_root, &progress_w);
        *result_w.lock().unwrap() = Some(outcome);
        done_w.store(true, Ordering::SeqCst);
    });

    AutoIndexHandle {
        progress,
        done,
        result,
        handle: Some(handle),
    }
}

// ── Pipeline ────────────────────────────────────────────────────────────────

fn run_manifest_pipeline(
    store: &Arc<Store>,
    root: &Path,
    progress: &Arc<Mutex<AutoIndexProgress>>,
) -> anyhow::Result<()> {
    // ── Acquire file lock (prevents concurrent atlas processes) ──
    let _lock =
        FileLock::acquire(store).context("Another atlas process is indexing this project.")?;

    let empty_patterns: Vec<String> = vec![];

    // ── Phase 1: Discovery ────────────────────────────────────────────
    set_phase(progress, "Discovery", 0, 0, "Scanning project files...");
    let discovered = atlas_engine::phase_discover(root, &empty_patterns, &empty_patterns)
        .context("Failed to discover files")?;

    if discovered.is_empty() {
        // No source files — nothing to index.
        atlas_engine::phase_finalize(store, root, &empty_patterns)?;
        return Ok(());
    }

    // ── Phase 2: HashCheck ────────────────────────────────────────────
    let hash_result = atlas_engine::phase_dirty_check(store, &discovered, root)?;
    let dirty = &hash_result.dirty;
    let reused = hash_result.clean_count;
    set_phase(
        progress,
        "HashCheck",
        0,
        discovered.len() as u64,
        &format!("{} dirty / {} reused", dirty.len(), reused),
    );

    // ── Phase 3: Cleanup stale ────────────────────────────────────────
    if !hash_result.deleted.is_empty() {
        let deleted_count = hash_result.deleted.len();
        set_phase(
            progress,
            "Cleanup",
            0,
            deleted_count as u64,
            &format!("Removing {deleted_count} stale files"),
        );
        atlas_engine::phase_cleanup_stale(store, &hash_result.deleted)?;
    }

    if dirty.is_empty() {
        // Everything reused — just finalize.
        atlas_engine::phase_finalize(store, root, &empty_patterns)?;
        return Ok(());
    }

    // ── Phase 4: Language init ────────────────────────────────────────
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

    set_phase(
        progress,
        "LanguageInit",
        0,
        0,
        &format!("{} languages", languages.len()),
    );

    let _registry = LanguageRegistry::new(&languages).or_else(|e| {
        let available: Vec<Language> = languages
            .iter()
            .filter(|l| LanguageRegistry::new(&[**l]).is_ok())
            .copied()
            .collect();
        if available.is_empty() {
            Err(e)
        } else {
            LanguageRegistry::new(&available)
        }
    })?;

    let frontend_cache: HashMap<Language, LanguageFrontend> = languages
        .iter()
        .filter_map(|&lang| atlas_engine::create_frontend(lang).map(|fe| (lang, fe)))
        .collect();

    // ── Phase 5: Extraction (manifest mode, parallel) ─────────────────
    set_phase(
        progress,
        "Extraction",
        0,
        dirty.len() as u64,
        &format!("{} files (manifest)", dirty.len()),
    );

    let pool = ParseWorkerPool::new(WorkerConfig::default());
    let extracted_count = AtomicUsize::new(0);
    let per_lang_mutex = Mutex::new(PerLanguageStats::new());
    let fc = &frontend_cache;
    let prog = progress;

    let results: Vec<_> = dirty
        .par_iter()
        .filter_map(|rel_path| {
            let abs_path = root.join(rel_path);
            let lang = Language::from_path(rel_path)?;
            let frontend = fc.get(&lang)?;
            let file_start = Instant::now();
            let result = crate::runtime::extract_one(
                &pool,
                &abs_path,
                root,
                lang,
                frontend,
                ExtractionMode::Manifest,
            );
            let extract_ms = file_start.elapsed().as_millis() as u64;

            let count = extracted_count.fetch_add(1, Ordering::Relaxed);
            // Update progress every 50 files to avoid mutex contention.
            if count % 50 == 0 {
                if let Ok(mut p) = prog.lock() {
                    p.current = count as u64;
                }
            }

            let (facts_opt, failed, fail_cat) = match result {
                Ok(facts) => (Some(facts), false, None),
                Err(ref _e) => (None, true, Some("extraction_error")),
            };
            {
                per_lang_mutex
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record_file(lang, extract_ms, failed, fail_cat);
            }
            facts_opt.map(|facts| ExtractedFile {
                rel_path: rel_path.clone(),
                language: lang,
                facts,
            })
        })
        .collect();

    let extracted = results;
    let extracted_count = extracted.len();

    // ── Phase 6: Clean stale facts before re-insert ───────────────────
    set_phase(
        progress,
        "Cleanup",
        0,
        0,
        &format!("{extracted_count} re-indexed"),
    );
    let file_ids: Vec<_> = extracted.iter().map(|ef| ef.facts.file.file_id).collect();
    atlas_engine::phase_cleanup_file_ids(store, &file_ids)
        .context("Failed to clean stale facts")?;

    // ── Phase 7: DB Write (serial, batched) ───────────────────────────
    let dirty_total = dirty.len();
    set_phase(
        progress,
        "DbWrite",
        0,
        extracted_count as u64,
        &format!("Storing {extracted_count} files"),
    );

    let extracted_files = ExtractedFiles {
        items: extracted,
        stats: ExtractionPhaseStats {
            attempted: dirty_total,
            succeeded: extracted_count,
            failed: dirty_total.saturating_sub(extracted_count),
            symbols: 0,
        },
    };

    let prog2 = progress;
    let _write_stats = atlas_engine::phase_write_batched(
        store,
        &extracted_files,
        500,
        500,
        |written| {
            if let Ok(mut p) = prog2.lock() {
                p.current = written;
            }
        },
        || false, // not interruptible
    )?;

    // ── Phase 8: Finalize ─────────────────────────────────────────────
    set_phase(progress, "Finalize", 0, 0, "");
    atlas_engine::phase_finalize(store, root, &empty_patterns)?;

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn set_phase(
    progress: &Arc<Mutex<AutoIndexProgress>>,
    phase: &str,
    current: u64,
    total: u64,
    message: &str,
) {
    if let Ok(mut p) = progress.lock() {
        p.phase = phase.to_string();
        p.current = current;
        p.total = total;
        p.message = message.to_string();
    }
}
