//! Auto-index: runs a manifest-mode index pipeline in a background thread
//! on TUI startup when the database is empty.
//!
//! ## Design
//! - **Worker thread**: runs the manifest pipeline via [`IndexPipeline`] and
//!   updates [`AutoIndexProgress`] through an [`AutoIndexProgressSink`].
//! - **Main thread**: polls `progress` every tick and renders a progress screen.
//! - **Pipeline**: driven by [`IndexPipeline`] which orchestrates all phases
//!   (Discovery → HashCheck → Cleanup → LanguageInit → Extraction → DbWrite →
//!   Finalize).
//! - **Cancellable**: Press Esc in TUI to set the cancel flag; worker exits at
//!   next phase boundary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Context;
use atlas_engine::{
    ExtractionMode, FileLock, IndexPipeline, IndexPipelineOptions, ProgressEvent, ProgressSink,
    Store,
};

// ── Progress ────────────────────────────────────────────────────────────────

/// Progress visible to the main TUI thread.
///
/// Updated by the worker thread via [`AutoIndexProgressSink`]; polled by the
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

// ── Progress Sink ───────────────────────────────────────────────────────────

/// Bridges [`ProgressEvent`]s from [`IndexPipeline`] into [`AutoIndexProgress`].
struct AutoIndexProgressSink {
    progress: Arc<Mutex<AutoIndexProgress>>,
}

impl ProgressSink for AutoIndexProgressSink {
    fn emit(&self, event: ProgressEvent) {
        match event {
            ProgressEvent::PhaseStarted { phase, total } => {
                if let Ok(mut p) = self.progress.lock() {
                    p.phase = phase.to_string();
                    p.total = total;
                    p.current = 0;
                }
            }
            ProgressEvent::ItemProgress { completed, .. } => {
                if let Ok(mut p) = self.progress.lock() {
                    p.current = completed;
                }
            }
            ProgressEvent::PhaseFinished { detail, .. } => {
                if let Ok(mut p) = self.progress.lock() {
                    p.message = detail.unwrap_or_default();
                }
            }
            ProgressEvent::Warning { .. } => {
                // non-fatal, silently continue
            }
            ProgressEvent::Cancelled { .. } => {
                // cancellation handled via the cancel token / interrupt closure
            }
        }
    }
}

// ── Handle ──────────────────────────────────────────────────────────────────

/// Handle returned by [`spawn_auto_index`].
///
/// The caller polls `progress` for UI updates and checks `done` to know
/// when the worker has finished.  Set `cancel` to `true` to request
/// cancellation at the next phase boundary.  After `done` is `true`, call
/// [`AutoIndexHandle::take_result`] to retrieve the outcome and join the
/// background thread.
pub struct AutoIndexHandle {
    /// Shared progress state — read from the TUI render loop.
    pub progress: Arc<Mutex<AutoIndexProgress>>,
    /// Set to `true` when the worker thread exits (success or error).
    pub done: Arc<AtomicBool>,
    /// Set to `true` by the TUI to request cancellation.
    pub cancel: Arc<AtomicBool>,
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
/// Set [`AutoIndexHandle::cancel`] to request cancellation at the next
/// phase boundary.
pub fn spawn_auto_index(store: Arc<Store>, project_root: PathBuf) -> AutoIndexHandle {
    let progress = Arc::new(Mutex::new(AutoIndexProgress {
        phase: "Initializing".into(),
        current: 0,
        total: 0,
        message: String::new(),
    }));
    let done = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    let result: Arc<Mutex<Option<anyhow::Result<()>>>> = Arc::new(Mutex::new(None));

    let progress_w = Arc::clone(&progress);
    let done_w = Arc::clone(&done);
    let cancel_w = Arc::clone(&cancel);
    let result_w = Arc::clone(&result);

    let handle = std::thread::spawn(move || {
        let outcome = run_manifest_pipeline(&store, &project_root, &progress_w, &cancel_w);
        *result_w.lock().unwrap() = Some(outcome);
        done_w.store(true, Ordering::SeqCst);
    });

    AutoIndexHandle {
        progress,
        done,
        cancel,
        result,
        handle: Some(handle),
    }
}

// ── Pipeline ────────────────────────────────────────────────────────────────

fn run_manifest_pipeline(
    store: &Arc<Store>,
    root: &Path,
    progress: &Arc<Mutex<AutoIndexProgress>>,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    // ── Acquire file lock (prevents concurrent atlas processes) ──
    let _lock =
        FileLock::acquire(store).context("Another atlas process is indexing this project.")?;

    let empty_patterns: Vec<String> = vec![];

    let options = IndexPipelineOptions::new(ExtractionMode::Manifest)
        .with_include_patterns(empty_patterns.clone())
        .with_exclude_patterns(empty_patterns.clone());

    let pipeline = IndexPipeline::new(Arc::clone(store), root.to_path_buf(), options);
    let sink = AutoIndexProgressSink {
        progress: Arc::clone(progress),
    };

    let cancel_clone = Arc::clone(cancel);
    let mut interrupted = move || cancel_clone.load(Ordering::Relaxed);
    let _stats = pipeline.run(&sink, &mut interrupted)?;

    Ok(())
}
