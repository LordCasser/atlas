//! FocusRuntime — single control-plane entry point for all MCP focus/lazy queries.
//!
//! # Architecture
//!
//! FocusRuntime detects whether the project has a full index. If so, it returns
//! `IndexMode::FullIndex` so MCP tools can use the existing resolution + graph
//! code path. If not, it orchestrates:
//!
//! ```text
//!   bootstrap → seed locate → closure build → scoped resolve → scoped graph
//! ```
//!
//! # Concurrency
//!
//! - Synchronous minimal closure build (user-blocking) happens in `prepare()`.
//! - Background expansion is enqueued via [`FocusScheduler`] for pre-warming.
//! - All DB writes are serialized through the scheduler's write coordinator.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use db::Store;
use types::enums::Language;
use types::ids::{FileId, SymbolId};
use types::structs::{
    CoverageTier, KnownGap, Precision, SemanticConfidence, SymbolTier,
};

use crate::lazy_structural::{CandidateProvider, DefaultCandidateProvider, LazyStructuralService};

use super::bootstrap::BootstrapManager;
use super::engine::ClosureEngine;
use super::query::QueryIntent;
use super::scheduler::{self, FocusPriority, FocusScheduler};
use super::types::{
    ClosureStrategy, FocusSeed, FocusWindow, WindowBudget,
};

// ── Metadata keys for full-index detection ──────────────────────────────────

/// Key set when a full resolution pass has completed.
const KEY_RESOLUTION_GENERATION: &str = "resolution_generation_version";
/// Key set with the config hash after resolution.
const KEY_RESOLUTION_CONFIG_HASH: &str = "resolution_config_hash";

// ── IndexMode ───────────────────────────────────────────────────────────────

/// Whether the project has a full index or needs focus-driven analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    /// Full index exists — use existing resolution + graph code path.
    FullIndex,
    /// No full index — use focus-driven incremental analysis.
    Focus,
}

// ── FocusResult ─────────────────────────────────────────────────────────────

/// Result of a focus query preparation.
///
/// MCP tools use this to decide which code path to take and what precision
/// guarantees can be made.
#[derive(Debug, Clone)]
pub struct FocusResult {
    /// The index mode. MCP uses this to decide which code path to take.
    pub mode: IndexMode,
    /// For Focus mode: precision of the returned results.
    pub precision: Option<Precision>,
    /// For Focus mode: known gaps.
    pub gaps: Vec<KnownGap>,
    /// For Focus mode: pending closure IDs being built in background.
    pub pending_closure_ids: Vec<String>,
    /// For Focus mode: the closure_id if a closure was built.
    pub closure_id: Option<String>,
    /// For Focus mode: seed symbol_id if located.
    pub seed_symbol_id: Option<SymbolId>,
    /// For Focus mode: seed file_id.
    pub seed_file_id: Option<FileId>,
    /// For Focus mode: FileIds that were structurally built during preparation.
    pub built_files: Vec<FileId>,
    /// For Focus mode: distribution of results by coverage tier
    /// (e.g. {"ClosureComplete": 8, "Boundary": 5, "Manifest": 12}).
    /// None in FullIndex mode or when coverage data is unavailable.
    pub coverage_counts: Option<HashMap<String, usize>>,
}

// ── FocusRuntime ────────────────────────────────────────────────────────────

/// Single control-plane entry point for MCP focus/lazy queries.
///
/// Detects index mode, orchestrates bootstrap, seed location, closure building,
/// and background expansion for focus-driven incremental analysis.
pub struct FocusRuntime {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    bootstrap: BootstrapManager,
    scheduler: Arc<std::sync::Mutex<FocusScheduler>>,
    closure_engine: Option<ClosureEngine>,
    started: AtomicBool,
    /// Join handle for the background worker thread (if spawned).
    bg_handle: Option<JoinHandle<()>>,
}

impl FocusRuntime {
    // ── Construction ─────────────────────────────────────────────────────

    /// Create a new FocusRuntime. Does NOT start bootstrap.
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        Self {
            store: store.clone(),
            project_root: project_root.clone(),
            bootstrap: BootstrapManager::new(store.clone(), project_root.clone()),
            scheduler: Arc::new(std::sync::Mutex::new(FocusScheduler::new(store))),
            closure_engine: None,
            started: AtomicBool::new(false),
            bg_handle: None,
        }
    }

    // ── Index mode detection ─────────────────────────────────────────────

    /// Detect whether a full index exists.
    ///
    /// A full index is detected by checking `project_metadata` for:
    /// - `resolution_config_hash` — set after resolution completes.
    /// - `resolution_generation_version` with a non-zero value — set after
    ///   at least one full resolution pass.
    ///
    /// A schema with `resolution_generation_version = '0'` (default after
    /// `init_schema`) is treated as Focus mode.
    pub fn detect_index_mode(&self) -> IndexMode {
        let has_config_hash = self.store
            .get_metadata(KEY_RESOLUTION_CONFIG_HASH)
            .ok()
            .flatten()
            .is_some();

        if has_config_hash {
            return IndexMode::FullIndex;
        }

        let generation = self.store
            .get_metadata(KEY_RESOLUTION_GENERATION)
            .ok()
            .flatten();

        match generation {
            Some(ref v) if v != "0" => IndexMode::FullIndex,
            _ => IndexMode::Focus,
        }
    }

    // ── Main entry point ─────────────────────────────────────────────────

    /// Prepare for a query. This is the main entry point.
    ///
    /// For **FullIndex** mode: returns `FocusResult` with `mode = FullIndex`
    /// immediately — the caller should use the existing resolution + graph path.
    ///
    /// For **Focus** mode: ensures bootstrap minimum, locates the seed, builds
    /// a minimal closure synchronously, then enqueues background expansion.
    pub fn prepare(&mut self, intent: &QueryIntent) -> Result<FocusResult> {
        let mode = self.detect_index_mode();

        if mode == IndexMode::FullIndex {
            return Ok(FocusResult {
                mode: IndexMode::FullIndex,
                precision: None,
                gaps: Vec::new(),
                pending_closure_ids: Vec::new(),
                closure_id: None,
                seed_symbol_id: None,
                seed_file_id: None,
                built_files: Vec::new(),
                coverage_counts: None,
            });
        }

        // ── Focus path ──────────────────────────────────────────────────

        // 1. Ensure bootstrap is started and minimum tier is ready
        self.ensure_started();
        self.bootstrap.ensure_minimum_ready();

        // 2. Locate seed
        let (seed, seed_file_id, seed_symbol_id, language) = self.locate_seed(intent)?;

        // 3. Ensure closure engine exists
        self.ensure_closure_engine()?;

        // 4. Build minimal closure synchronously
        let minimal_window = FocusWindow {
            seed: seed.clone(),
            strategies: vec![ClosureStrategy::ImportNeighborhood { depth: 1 }],
            budget: WindowBudget::default(),
            language,
            max_iterations: 1,
        };

        let closure_id = scheduler::next_job_id();
        let engine = self.closure_engine.as_ref()
            .context("ClosureEngine not initialized")?;
        let closure = engine.build_closure(&minimal_window, &closure_id)?;

        // 5. Build FocusResult with precision from the closure
        let precision = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Manifest,
            },
            confidence: SemanticConfidence::Medium,
        };

        let built_files: Vec<FileId> = closure.files.iter().copied().collect();

        // 5b. Compute coverage distribution from the closure_coverage table.
        let coverage_counts: Option<HashMap<String, usize>> = self
            .store
            .get_coverage_counts(&closure_id)
            .ok()
            .map(|rows| {
                rows.into_iter()
                    .map(|(source, count)| (map_coverage_source_to_tier(&source), count as usize))
                    .collect()
            });

        let gaps = closure.gaps.clone();
        let pending_closure_ids = vec![closure_id.clone()];

        // 6. Enqueue background expansion
        let bg_window = FocusWindow {
            seed,
            strategies: vec![
                ClosureStrategy::ImportNeighborhood { depth: 2 },
                ClosureStrategy::SameDirectory,
            ],
            budget: WindowBudget::background(),
            language,
            max_iterations: 3,
        };
        let bg_closure_id = self.scheduler
            .lock()
            .unwrap()
            .enqueue(bg_window, FocusPriority::UserFocus);

        let mut pending_ids = pending_closure_ids;
        pending_ids.push(bg_closure_id);

        Ok(FocusResult {
            mode: IndexMode::Focus,
            precision: Some(precision),
            gaps,
            pending_closure_ids: pending_ids,
            closure_id: Some(closure_id),
            seed_symbol_id,
            seed_file_id,
            built_files,
            coverage_counts,
        })
    }

    // ── Bootstrap lifecycle ──────────────────────────────────────────────

    /// Ensure the bootstrap manager has started and the background focus
    /// worker thread is running.
    ///
    /// Idempotent — second call is a no-op.
    pub fn ensure_started(&mut self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return; // already started
        }
        self.bootstrap.start();

        // Mark the scheduler as running so the background worker doesn't
        // exit immediately on its first loop iteration.
        {
            let mut s = self.scheduler.lock().unwrap();
            s.set_running(true);
        }

        // Start the background focus scheduler worker.
        // The scheduler processes jobs from ALL priority queues
        // (UserFocus, Recent, Speculative) for pre-warming.
        //
        // We spawn the thread here rather than in FocusScheduler::start_background
        // so that ensure_started() owns the JoinHandle and can shut down cleanly.
        let sched = Arc::clone(&self.scheduler);
        let handle = std::thread::Builder::new()
            .name("atlas-focus-bg".into())
            .spawn(move || {
                FocusScheduler::background_worker_loop(sched);
            })
            .expect("failed to spawn focus background worker thread");
        self.bg_handle = Some(handle);
    }

    /// Check if bootstrap minimum tier is ready.
    pub fn is_ready(&self) -> bool {
        self.bootstrap.is_minimum_ready()
    }

    /// Check if the scheduler has pending background jobs.
    pub fn has_pending_jobs(&self) -> bool {
        self.scheduler.lock().unwrap().has_pending()
    }

    /// Shut down the background worker thread.
    ///
    /// Sets the running flag to false and joins the thread. Idempotent.
    pub fn shutdown(&mut self) {
        // Signal the background worker to stop.
        if let Ok(s) = self.scheduler.lock() {
            s.stop_background();
        }
        // Join the thread if we have a handle.
        if let Some(handle) = self.bg_handle.take() {
            let _ = handle.join();
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// Locate the seed from a QueryIntent.
    ///
    /// Returns (FocusSeed, Option<FileId>, Option<SymbolId>, Language).
    fn locate_seed(
        &self,
        intent: &QueryIntent,
    ) -> Result<(FocusSeed, Option<FileId>, Option<SymbolId>, Language)> {
        match intent {
            QueryIntent::Calls { symbol_name, file_id, symbol_id } => {
                self.locate_calls_seed(symbol_name, file_id, symbol_id)
            }
            QueryIntent::TracePoint { file_id, line, column } => {
                let language = self.resolve_language_for_file(file_id)
                    .unwrap_or_default();
                let seed = FocusSeed::Position {
                    file_id: *file_id,
                    line: *line,
                    column: *column,
                };
                Ok((seed, Some(*file_id), None, language))
            }
        }
    }

    /// Locate seed for a Calls intent.
    fn locate_calls_seed(
        &self,
        symbol_name: &str,
        file_id: &Option<FileId>,
        symbol_id: &Option<SymbolId>,
    ) -> Result<(FocusSeed, Option<FileId>, Option<SymbolId>, Language)> {
        // Priority: symbol_id > file_id > candidate search
        if let Some(sym_id) = symbol_id {
            let sym = self.store
                .find_symbol_by_id(sym_id)?
                .with_context(|| format!("Symbol not found: {:?}", sym_id))?;
            let seed = FocusSeed::Symbol {
                name: sym.name.clone(),
                kind: Some(sym.kind),
                language: sym.language,
            };
            return Ok((seed, Some(sym.file_id), Some(*sym_id), sym.language));
        }

        if let Some(fid) = file_id {
            let language = self.resolve_language_for_file(fid).unwrap_or_default();
            let seed = FocusSeed::File {
                file_id: *fid,
                language,
            };
            return Ok((seed, Some(*fid), None, language));
        }

        // Candidate-based search using DefaultCandidateProvider
        let provider = DefaultCandidateProvider::new(
            self.store.clone(),
            self.project_root.clone(),
        );
        let candidates = provider
            .candidates_for_symbol(symbol_name)
            .context("Failed to locate candidates for symbol")?;

        let seed_file_id = candidates.first().copied();
        let language = seed_file_id
            .map(|fid| self.resolve_language_for_file(&fid).unwrap_or_default())
            .unwrap_or_default();

        let seed = FocusSeed::Symbol {
            name: symbol_name.to_string(),
            kind: None,
            language,
        };

        Ok((seed, seed_file_id, None, language))
    }

    /// Ensure the closure engine is initialized.
    ///
    /// Creates the engine lazily on first call so that `include_roots` can be
    /// configured after construction. Uses empty `include_roots` by default.
    ///
    /// Also sets a second engine instance on the scheduler so the background
    /// worker can process jobs from all priority queues.
    fn ensure_closure_engine(&mut self) -> Result<()> {
        if self.closure_engine.is_some() {
            return Ok(());
        }
        let lazy_structural = LazyStructuralService::new(
            self.store.clone(),
            self.project_root.clone(),
        );
        let engine = ClosureEngine::new(
            self.store.clone(),
            lazy_structural,
            self.project_root.clone(),
            vec![], // include_roots can be set later via engine configuration
        );

        // Create a second engine instance for the scheduler's background worker.
        let sched_lazy = LazyStructuralService::new(
            self.store.clone(),
            self.project_root.clone(),
        );
        let sched_engine = ClosureEngine::new(
            self.store.clone(),
            sched_lazy,
            self.project_root.clone(),
            vec![],
        );

        self.closure_engine = Some(engine);
        self.scheduler.lock().unwrap().set_engine(sched_engine);
        Ok(())
    }

    /// Resolve the language for a file by looking it up in the store.
    fn resolve_language_for_file(&self, file_id: &FileId) -> Option<Language> {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|fi| fi.language)
    }
}

// ── Drop ────────────────────────────────────────────────────────────────────

/// Map a raw `closure_coverage.source` value to a human-readable coverage tier name.
fn map_coverage_source_to_tier(source: &str) -> String {
    match source {
        "extracted_structural" => "ClosureComplete".to_string(),
        "extracted_resolution_symbols" => "Boundary".to_string(),
        "extracted_manifest" => "Manifest".to_string(),
        other => other.to_string(),
    }
}

impl Drop for FocusRuntime {
    fn drop(&mut self) {
        // Signal the background worker to stop.
        // Use a best-effort lock — if the lock is poisoned, the process is
        // already in a bad state and we should not panic during drop.
        if let Ok(s) = self.scheduler.lock() {
            s.stop_background();
        }
        // Join the background thread if we have a handle.
        if let Some(handle) = self.bg_handle.take() {
            let _ = handle.join();
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
