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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use db::Store;
use types::enums::Language;
use types::ids::{FileId, SymbolId};
use types::structs::{
    CapabilityMask, CoverageTier, KnownGap, Precision, SemanticConfidence, SymbolTier,
};

use crate::LazyDataflowService;
use crate::investigation::{Investigation, InvestigationFocus};
use crate::lazy_structural::{CandidateProvider, DefaultCandidateProvider, LazyStructuralService};

use super::bootstrap::BootstrapManager;
use super::engine::ClosureEngine;
use super::job_tracker::JobTracker;
use super::query::QueryIntent;
use super::scheduler::{self, FocusPriority, FocusScheduler};
use super::types::{ClosureStrategy, Direction, FocusSeed, FocusWindow, WindowBudget};

/// Maximum time a foreground MCP request should wait for initial file
/// inventory. Bootstrap continues in the background after this deadline.
const BOOTSTRAP_MIN_READY_WAIT_MS: u64 = 5_000;
const HOT_REGION_EXTENSION_FILE_BONUS: usize = 50;
const HOT_REGION_EXTENSION_DEPTH_CAP: u32 = 3;
/// Maximum number of independent hot regions kept in memory for
/// in-memory (non-persistent) stores.  When exceeded, the shallowest
/// and oldest region is evicted.  Persistent stores keep all regions
/// indefinitely since they are bounded by the project's natural
/// investigation breadth — an LRU would discard useful state that
/// can survive across sessions.
const MAX_MEMORY_HOT_REGIONS: usize = 10;

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
    /// (e.g. {"local_complete": 8, "boundary": 5, "basic": 12}).
    /// None in FullIndex mode or when coverage data is unavailable.
    pub coverage_counts: Option<HashMap<String, usize>>,
    /// Shared job tracker for checking background job completion.
    /// `None` in FullIndex mode (no background jobs are running).
    pub job_tracker: Option<Arc<JobTracker>>,
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
    /// Index mode override for testing. When `Some`, `detect_index_mode()`
    /// returns this value instead of calling `Store::read_index_mode()`.
    detect_index_mode_override: Option<IndexMode>,
    /// Optional shared dataflow service from the MCP analysis runtime.
    /// When set, ensure_closure_engine() uses this instead of creating
    /// a duplicate instance.  The scheduler background engine still
    /// creates its own instance (independent thread safety boundary).
    shared_lazy_dataflow: Option<LazyDataflowService>,
    /// Runtime-owned hot region state. The scheduler executes jobs; this
    /// tracker decides whether a new query is extending an existing closure.
    hot_regions: HotRegionTracker,
    /// Shared tracker for background job completion.
    /// Foreground jobs are marked done immediately; background
    /// jobs are marked by the scheduler on completion.
    job_tracker: Arc<JobTracker>,
}

// ── Hot region tracking ────────────────────────────────────────────────────

#[derive(Debug)]
struct HotRegionTracker {
    regions: Vec<HotRegion>,
    next_region_id: u64,
    /// When `true`, the backing store is persistent (disk-backed atlas.db).
    /// Persistent stores keep all hot regions indefinitely — investigations
    /// span sessions and evicting would discard useful state.
    /// When `false` (in-memory store), regions are bounded by
    /// [`MAX_MEMORY_HOT_REGIONS`] with LRU eviction.
    is_persistent: bool,
}

impl Default for HotRegionTracker {
    fn default() -> Self {
        // Default to persistent; FocusRuntime::new() sets this correctly
        // from Store::db_path().
        Self {
            regions: Vec::new(),
            next_region_id: 0,
            is_persistent: true,
        }
    }
}

#[derive(Debug)]
struct HotRegion {
    id: String,
    files: HashSet<FileId>,
    boundary_files: HashSet<FileId>,
    depth: u32,
    pending_closure_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct BoundaryHit {
    region_id: String,
    depth: u32,
}

impl HotRegionTracker {
    fn boundary_hit(&self, seed_file_id: Option<FileId>) -> Option<BoundaryHit> {
        let seed_file_id = seed_file_id?;
        self.boundary_hit_for_file(seed_file_id)
    }

    fn boundary_hit_for_files(&self, file_ids: &[FileId]) -> Option<BoundaryHit> {
        file_ids
            .iter()
            .find_map(|file_id| self.boundary_hit_for_file(*file_id))
    }

    fn boundary_hit_for_file(&self, file_id: FileId) -> Option<BoundaryHit> {
        self.regions
            .iter()
            .find(|region| {
                region.boundary_files.contains(&file_id) || region.files.contains(&file_id)
            })
            .map(|region| BoundaryHit {
                region_id: region.id.clone(),
                depth: region.depth,
            })
    }

    fn observe_closure(
        &mut self,
        seed_file_id: Option<FileId>,
        built_files: &[FileId],
        pending_closure_ids: &[String],
        expanded_existing_region: bool,
    ) -> Option<String> {
        if seed_file_id.is_none() && built_files.is_empty() {
            return None;
        }

        let mut region = match self.find_region_index(seed_file_id, built_files) {
            Some(index) => self.regions.remove(index),
            None => {
                let id = format!("hr_{}", self.next_region_id);
                self.next_region_id += 1;
                HotRegion {
                    id,
                    files: HashSet::new(),
                    boundary_files: HashSet::new(),
                    depth: 0,
                    pending_closure_ids: Vec::new(),
                }
            }
        };

        if expanded_existing_region {
            region.depth = region.depth.saturating_add(1).max(1);
        } else {
            region.depth = region.depth.max(1);
        }

        if let Some(file_id) = seed_file_id {
            region.files.insert(file_id);
            region.boundary_files.insert(file_id);
        }
        for file_id in built_files {
            region.files.insert(*file_id);
            region.boundary_files.insert(*file_id);
        }
        for closure_id in pending_closure_ids {
            if !region.pending_closure_ids.contains(closure_id) {
                region.pending_closure_ids.push(closure_id.clone());
            }
        }

        let region_id = region.id.clone();
        self.regions.push(region);

        // In-memory stores keep only a bounded set of hot regions. The Vec
        // order is the LRU order: every touched region is moved to the back
        // above, and eviction chooses the shallowest oldest region while
        // preserving the just-touched region.
        if !self.is_persistent && self.regions.len() > MAX_MEMORY_HOT_REGIONS {
            let touched_idx = self.regions.len() - 1;
            let evict_idx = self
                .regions
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != touched_idx)
                .min_by_key(|(idx, region)| (region.depth, *idx))
                .map(|(idx, _)| idx);

            if let Some(idx) = evict_idx {
                tracing::info!(
                    region_id = %self.regions[idx].id,
                    depth = self.regions[idx].depth,
                    remaining = self.regions.len() - 1,
                    "evicting LRU hot region (in-memory mode)"
                );
                self.regions.remove(idx);
            }
        }

        Some(region_id)
    }

    fn find_region_index(
        &self,
        seed_file_id: Option<FileId>,
        built_files: &[FileId],
    ) -> Option<usize> {
        self.regions.iter().position(|region| {
            seed_file_id.is_some_and(|file_id| {
                region.boundary_files.contains(&file_id) || region.files.contains(&file_id)
            }) || built_files.iter().any(|file_id| {
                region.boundary_files.contains(file_id) || region.files.contains(file_id)
            })
        })
    }
}

impl FocusRuntime {
    // ── Construction ─────────────────────────────────────────────────────

    /// Create a new FocusRuntime. Does NOT start bootstrap.
    ///
    /// Detects whether the backing store is persistent (on-disk atlas.db)
    /// or in-memory, and configures the hot region tracker accordingly:
    /// persistent stores keep all hot regions indefinitely, while
    /// in-memory stores evict the shallowest/LRU region beyond
    /// [`MAX_MEMORY_HOT_REGIONS`].
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        let is_persistent = store.db_path().to_string_lossy() != ":memory:";
        let job_tracker = Arc::new(JobTracker::new());
        Self {
            store: store.clone(),
            project_root: project_root.clone(),
            bootstrap: BootstrapManager::new(store.clone(), project_root.clone()),
            scheduler: Arc::new(std::sync::Mutex::new(
                FocusScheduler::new(store).with_job_tracker(Arc::clone(&job_tracker))
            )),
            closure_engine: None,
            started: AtomicBool::new(false),
            bg_handle: None,
            detect_index_mode_override: None,
            shared_lazy_dataflow: None,
            hot_regions: HotRegionTracker {
                is_persistent,
                ..HotRegionTracker::default()
            },
            job_tracker,
        }
    }

    /// Share an external [`LazyDataflowService`] with the focus runtime.
    ///
    /// When set, `ensure_closure_engine()` uses this instance for the main
    /// closure engine instead of creating a duplicate.  The scheduler's
    /// background engine still gets its own copy for thread safety.
    pub fn with_lazy_dataflow(&mut self, svc: LazyDataflowService) -> &mut Self {
        self.shared_lazy_dataflow = Some(svc);
        self
    }

    // ── Index mode detection ─────────────────────────────────────────────

    /// Detect whether the project has a CLI-finalized rich index or operates
    /// in incremental Focus mode.
    ///
    /// Focus can write rich layers for a small closure. Those rows are useful
    /// cache entries, but they are not proof that the repository is fully
    /// indexed. A full-index decision therefore requires both fresh rich
    /// extraction state and index-finalization metadata.
    pub fn detect_index_mode(&self) -> IndexMode {
        if let Some(mode) = self.detect_index_mode_override {
            return mode;
        }
        let rich = self
            .store
            .read_index_mode()
            .is_ok_and(|mode| crate::is_rich_index_mode(&mode));
        let finalized = self
            .store
            .get_metadata("last_index_time")
            .ok()
            .flatten()
            .is_some();
        if rich && finalized {
            IndexMode::FullIndex
        } else {
            IndexMode::Focus
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
                job_tracker: None,
            });
        }

        // ── Focus path ──────────────────────────────────────────────────

        // 1. Ensure bootstrap is started and minimum tier is ready
        self.ensure_started();
        let bootstrap_ready = self
            .bootstrap
            .wait_minimum_ready(Duration::from_millis(BOOTSTRAP_MIN_READY_WAIT_MS));

        // 2. Locate seed
        let (seed, seed_file_id, seed_symbol_id, language) = self.locate_seed(intent)?;
        let mut boundary_hit = self.hot_regions.boundary_hit(seed_file_id);

        // 3. Ensure closure engine exists
        self.ensure_closure_engine()?;

        // 4. Build minimal closure synchronously
        //
        // TODO: the strategies match below is duplicated with bg_window (L296).
        // The two blocks differ only in numeric parameters (depth, budget,
        // iterations).  Extract strategies_for(intent, WindowKind) when the
        // third intent variant is added.
        let minimal_window = FocusWindow {
            seed: seed.clone(),
            strategies: match intent {
                QueryIntent::Calls { direction, .. } => {
                    let call_dir = match direction.as_deref() {
                        Some("incoming") => Direction::Incoming,
                        Some("outgoing") => Direction::Outgoing,
                        _ => Direction::Both,
                    };
                    vec![
                        ClosureStrategy::CallGraph {
                            direction: call_dir,
                            depth: 2,
                        },
                        ClosureStrategy::ImportNeighborhood { depth: 1 },
                    ]
                }
                QueryIntent::Context { .. } => vec![
                    ClosureStrategy::CallGraph {
                        direction: Direction::Both,
                        depth: 2,
                    },
                    ClosureStrategy::TypeGraph { max_depth: 1 },
                    ClosureStrategy::ImportNeighborhood { depth: 1 },
                ],
                QueryIntent::Explore { .. } => vec![
                    ClosureStrategy::CallGraph {
                        direction: Direction::Both,
                        depth: 2,
                    },
                    ClosureStrategy::ImportNeighborhood { depth: 1 },
                ],
                QueryIntent::Path { .. } => vec![
                    ClosureStrategy::CallGraph {
                        direction: Direction::Both,
                        depth: 3,
                    },
                    ClosureStrategy::ImportNeighborhood { depth: 2 },
                ],
                QueryIntent::Impact { .. } => {
                    vec![ClosureStrategy::ImportNeighborhood { depth: 2 }]
                }
                QueryIntent::Search { .. } => vec![
                    ClosureStrategy::ImportNeighborhood { depth: 1 },
                    ClosureStrategy::SameDirectory,
                ],
                QueryIntent::TracePoint { .. } | QueryIntent::TraceVariable { .. } => vec![
                    ClosureStrategy::ImportNeighborhood { depth: 1 },
                    ClosureStrategy::SameDirectory,
                ],
            },
            budget: WindowBudget::default(),
            language,
            max_iterations: 1,
        };

        let closure_id = scheduler::next_job_id();
        let engine = self
            .closure_engine
            .as_ref()
            .context("ClosureEngine not initialized")?;
        let closure = engine.build_closure(&minimal_window, &closure_id)?;

        // 5. Compute coverage distribution from the closure_coverage table.
        let coverage_counts: Option<HashMap<String, usize>> = self
            .store
            .get_coverage_counts(&closure_id)
            .ok()
            .map(|rows| {
                rows.into_iter()
                    .map(|(source, count)| (map_coverage_source_to_tier(&source), count as usize))
                    .collect()
            });

        // 5b. Derive precision from actual coverage results.
        let total_coverage_items: usize = coverage_counts
            .as_ref()
            .map(|c| c.values().sum())
            .unwrap_or(0);

        let precision = if total_coverage_items > 0 {
            Precision {
                coverage: CoverageTier::ClosureComplete {
                    closure_id: closure_id.clone(),
                },
                confidence: SemanticConfidence::High,
            }
        } else {
            Precision {
                coverage: CoverageTier::Boundary {
                    target_tier: SymbolTier::Manifest,
                },
                confidence: SemanticConfidence::Low,
            }
        };

        let built_files: Vec<FileId> = closure.files.iter().copied().collect();
        if boundary_hit.is_none() {
            boundary_hit = self.hot_regions.boundary_hit_for_files(&built_files);
        }

        let mut gaps = closure.gaps.clone();
        if !bootstrap_ready {
            gaps.push(KnownGap::BudgetExhausted {
                strategy: "bootstrap_tier0_wait".to_string(),
                remaining: 0,
            });
        }
        // Mark foreground closure as immediately done in the tracker.
        self.job_tracker.mark_done(&closure_id);
        let pending_closure_ids: Vec<String> = Vec::new();

        // 6. Enqueue background expansion
        let bg_window = FocusWindow {
            seed,
            strategies: match intent {
                QueryIntent::Calls { direction, .. } => {
                    let call_dir = match direction.as_deref() {
                        Some("incoming") => Direction::Incoming,
                        Some("outgoing") => Direction::Outgoing,
                        _ => Direction::Both,
                    };
                    vec![
                        ClosureStrategy::CallGraph {
                            direction: call_dir,
                            depth: 3,
                        },
                        ClosureStrategy::ImportNeighborhood { depth: 2 },
                    ]
                }
                QueryIntent::Explore { .. } | QueryIntent::Context { .. } => vec![
                    ClosureStrategy::CallGraph {
                        direction: Direction::Both,
                        depth: 3,
                    },
                    ClosureStrategy::TypeGraph { max_depth: 2 },
                    ClosureStrategy::ImportNeighborhood { depth: 2 },
                ],
                QueryIntent::Path { .. } => vec![
                    ClosureStrategy::CallGraph {
                        direction: Direction::Both,
                        depth: 4,
                    },
                    ClosureStrategy::ImportNeighborhood { depth: 3 },
                ],
                QueryIntent::Impact { .. } => {
                    vec![ClosureStrategy::ImportNeighborhood { depth: 3 }]
                }
                QueryIntent::Search { .. } => vec![
                    ClosureStrategy::ImportNeighborhood { depth: 2 },
                    ClosureStrategy::SameDirectory,
                ],
                QueryIntent::TracePoint { .. } | QueryIntent::TraceVariable { .. } => vec![
                    ClosureStrategy::ImportNeighborhood { depth: 2 },
                    ClosureStrategy::SameDirectory,
                ],
            },
            budget: WindowBudget::background(),
            language,
            max_iterations: 3,
        };
        let bg_closure_id = self
            .scheduler
            .lock()
            .unwrap()
            .enqueue(bg_window.clone(), FocusPriority::UserFocus);

        let mut pending_ids = pending_closure_ids;
        pending_ids.push(bg_closure_id);
        if let Some(hit) = boundary_hit.as_ref() {
            let extension_window = hot_region_extension_window(&bg_window, hit);
            let extension_closure_id = self
                .scheduler
                .lock()
                .unwrap()
                .enqueue(extension_window, FocusPriority::UserFocus);
            pending_ids.push(extension_closure_id);
        }

        self.hot_regions.observe_closure(
            seed_file_id,
            &built_files,
            &pending_ids,
            boundary_hit.is_some(),
        );

        // 7. Pre-warm investigation for the built files so their import
        //    neighborhoods are ready before the user queries them.
        if !built_files.is_empty() {
            let investigation = Investigation {
                focus: InvestigationFocus::Position {
                    file_id: built_files[0],
                    line: 0,
                    col: 0,
                },
                related_symbols: Vec::new(),
                related_files: built_files.clone(),
                desired_capabilities: CapabilityMask::from_bits(
                    CapabilityMask::MANIFEST | CapabilityMask::STRUCTURAL,
                ),
            };
            self.scheduler
                .lock()
                .unwrap()
                .prewarm_investigation(&investigation);
        }

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
            job_tracker: Some(Arc::clone(&self.job_tracker)),
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

    /// Check if Tier 0 (file_inventory) bootstrap is complete.
    pub fn is_tier0_complete(&self) -> bool {
        self.bootstrap.is_tier0_complete()
    }

    /// Check if Tier 1 (symbol hints for hot files) bootstrap is complete.
    pub fn is_tier1_hot_complete(&self) -> bool {
        self.bootstrap.is_tier1_hot_complete()
    }

    /// Number of files extracted during Tier 2 bootstrap.
    pub fn tier2_extracted(&self) -> u64 {
        self.bootstrap.tier2_extracted()
    }

    /// Whether the bootstrap/background worker has been started.
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Access the underlying store for DB queries.
    pub fn db_store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Check if the scheduler has pending background jobs.
    pub fn has_pending_jobs(&self) -> bool {
        self.scheduler.lock().unwrap().has_pending()
    }

    /// Enqueue file-focused background warming without building a foreground closure.
    ///
    /// Search uses this after returning a fast partial result: the current
    /// request stays responsive, while the focus scheduler parses the remaining
    /// hot files for a likely follow-up query.
    pub fn enqueue_file_focus_warm(&mut self, file_ids: &[FileId]) -> Result<Vec<String>> {
        if file_ids.is_empty() || self.detect_index_mode() == IndexMode::FullIndex {
            return Ok(Vec::new());
        }

        self.ensure_closure_engine()?;
        self.ensure_started();

        let mut seen = std::collections::HashSet::new();
        let mut job_ids = Vec::new();
        let mut scheduler = self.scheduler.lock().unwrap();
        for file_id in file_ids {
            if !seen.insert(*file_id) {
                continue;
            }
            let language = self.resolve_language_for_file(file_id).unwrap_or_default();
            let window = FocusWindow {
                seed: FocusSeed::File {
                    file_id: *file_id,
                    language,
                },
                strategies: vec![
                    ClosureStrategy::ImportNeighborhood { depth: 2 },
                    ClosureStrategy::SameDirectory,
                ],
                budget: WindowBudget::background(),
                language,
                max_iterations: 2,
            };
            job_ids.push(scheduler.enqueue(window, FocusPriority::UserFocus));
        }
        Ok(job_ids)
    }

    /// Called when a file is structurally ensured — pre-warm its imports
    /// so the import neighborhood is ready before the user queries it.
    pub fn on_file_read(&self, file_id: FileId) {
        self.scheduler.lock().unwrap().on_file_read(file_id);
    }

    /// Get queue depths by priority.
    pub fn queue_depths(&self) -> Vec<(FocusPriority, usize)> {
        self.scheduler.lock().unwrap().queue_depths()
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
            QueryIntent::Calls {
                symbol_name,
                file_id,
                symbol_id,
                ..
            } => self.locate_calls_seed(symbol_name, file_id, symbol_id),
            QueryIntent::Path { from_name, .. } => {
                // Use the "from" symbol as the seed for path queries.
                self.locate_calls_seed(from_name, &None, &None)
            }
            QueryIntent::Impact { symbol_name, .. } => {
                // Use the symbol name as the seed for impact analysis.
                self.locate_calls_seed(symbol_name, &None, &None)
            }
            QueryIntent::Explore {
                symbol_name,
                file_id,
                symbol_id,
            } => {
                // Explore uses the same symbol-based seed location as Calls
                self.locate_calls_seed(symbol_name, file_id, symbol_id)
            }
            QueryIntent::Context {
                symbol_name,
                file_id,
                symbol_id,
            } => {
                // Context uses the same symbol-based seed location as Calls
                self.locate_calls_seed(symbol_name, file_id, symbol_id)
            }
            QueryIntent::Search {
                query,
                scope: _scope,
            } => {
                // Search is also a seed-bearing query: use the actual query
                // text so the candidate provider can find matching files from
                // symbol hints, indexed symbols, or ripgrep + file_inventory.
                let language = Language::default();
                let seed = FocusSeed::Symbol {
                    name: query.clone(),
                    kind: None,
                    language,
                };
                Ok((seed, None, None, language))
            }
            QueryIntent::TracePoint {
                file_id,
                line,
                column,
            } => {
                let language = self.resolve_language_for_file(file_id).unwrap_or_default();
                let seed = FocusSeed::Position {
                    file_id: *file_id,
                    line: *line,
                    column: *column,
                };
                Ok((seed, Some(*file_id), None, language))
            }
            QueryIntent::TraceVariable {
                file_id,
                line,
                column,
            } => {
                // TraceVariable uses same position-based seed as TracePoint
                let language = self.resolve_language_for_file(file_id).unwrap_or_default();
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
            let sym = self
                .store
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
        let provider = DefaultCandidateProvider::new(self.store.clone(), self.project_root.clone());
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
        let lazy_structural =
            LazyStructuralService::new(self.store.clone(), self.project_root.clone());
        let lazy_dataflow = self.shared_lazy_dataflow.clone().unwrap_or_else(|| {
            let mut svc = LazyDataflowService::new(self.store.clone(), self.project_root.clone());
            // Wire up self-heal callback
            {
                let store_for_rebuild = self.store.clone();
                let root_for_rebuild = self.project_root.clone();
                svc.set_structural_rebuilder(std::sync::Arc::new(move |file_id| {
                    crate::lazy_structural::rebuild_structural_for_file(
                        &store_for_rebuild,
                        root_for_rebuild.as_deref(),
                        &file_id,
                    )
                }));
            }
            svc
        });
        let engine = ClosureEngine::new(
            self.store.clone(),
            lazy_structural,
            lazy_dataflow,
            self.project_root.clone(),
            vec![], // include_roots can be set later via engine configuration
        );

        // Create a second engine instance for the scheduler's background worker.
        let sched_lazy = LazyStructuralService::new(self.store.clone(), self.project_root.clone());
        let mut sched_dataflow =
            LazyDataflowService::new(self.store.clone(), self.project_root.clone());
        // Wire up self-heal callback
        {
            let store_for_rebuild = self.store.clone();
            let root_for_rebuild = self.project_root.clone();
            sched_dataflow.set_structural_rebuilder(std::sync::Arc::new(move |file_id| {
                crate::lazy_structural::rebuild_structural_for_file(
                    &store_for_rebuild,
                    root_for_rebuild.as_deref(),
                    &file_id,
                )
            }));
        }
        let sched_engine = ClosureEngine::new(
            self.store.clone(),
            sched_lazy,
            sched_dataflow,
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
        "extracted_structural" => "local_complete".to_string(),
        "extracted_resolution_symbols" => "boundary".to_string(),
        "extracted_manifest" => "basic".to_string(),
        other => other.to_string(),
    }
}

fn hot_region_extension_window(base: &FocusWindow, hit: &BoundaryHit) -> FocusWindow {
    let mut window = base.clone();
    let depth_bonus = hit.depth.clamp(1, HOT_REGION_EXTENSION_DEPTH_CAP);
    window.max_iterations = window.max_iterations.saturating_add(depth_bonus);
    window.budget.max_iterations = window.budget.max_iterations.saturating_add(1);
    window.budget.max_files = window
        .budget
        .max_files
        .saturating_add(HOT_REGION_EXTENSION_FILE_BONUS * depth_bonus as usize);
    tracing::debug!(
        hot_region_id = %hit.region_id,
        depth_bonus,
        max_files = window.budget.max_files,
        "enqueueing hot-region boundary expansion"
    );
    window
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
