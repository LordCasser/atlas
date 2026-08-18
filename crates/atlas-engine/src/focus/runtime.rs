//! FocusRuntime — single control-plane entry point for all MCP focus/lazy queries.
//!
//! # Architecture
//!
//! FocusRuntime detects whether the project has a full index. If so, it returns
//! `AccessStrategy::FullCache` so MCP tools can use the existing resolution + graph
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use db::Store;
use types::enums::Language;
use types::ids::{FileId, SymbolId};
use types::structs::{AnswerQuality, CoverageTier, KnownGap, SemanticConfidence, SymbolTier};

use crate::closure_planner::IncludeRoot;
use crate::focus::materialize::{CandidateProvider, DefaultCandidateProvider, FocusMaterialize};

use super::bootstrap::BootstrapManager;
use super::engine::ClosureEngine;
use super::job_tracker::JobTracker;
use super::query::{QueryIntent, QueryNeed};
use super::scheduler::{self, FocusPriority, FocusScheduler};
use super::types::{ClosureStrategy, Direction, FocusSeed, FocusWindow, WindowBudget};

/// Maximum time a foreground MCP request should wait for initial file
/// inventory. Bootstrap continues in the background after this deadline.
const BOOTSTRAP_MIN_READY_WAIT_MS: u64 = 5_000;
/// Maximum number of independent hot regions kept in memory for
/// in-memory (non-persistent) stores.  When exceeded, the shallowest
/// and oldest region is evicted.  Persistent stores keep all regions
/// indefinitely since they are bounded by the project's natural
/// investigation breadth — an LRU would discard useful state that
/// can survive across sessions.
const MAX_MEMORY_HOT_REGIONS: usize = 10;

fn call_direction(direction: Option<&str>) -> Direction {
    match direction {
        Some("incoming") => Direction::Incoming,
        Some("outgoing") => Direction::Outgoing,
        _ => Direction::Both,
    }
}

fn strategies_for(
    intent: &QueryIntent,
    background: bool,
    language: Language,
) -> Vec<ClosureStrategy> {
    let import = ClosureStrategy::ImportNeighborhood { depth: 1 };
    let type_depth = if background { 2 } else { 1 };
    match intent {
        QueryIntent::SemanticFunction { .. } => Vec::new(),
        QueryIntent::SymbolDetail { .. } => vec![import, ClosureStrategy::SameDirectory],
        QueryIntent::Calls { direction, .. } => vec![
            ClosureStrategy::CallGraph {
                direction: call_direction(direction.as_deref()),
                depth: 1,
            },
            import,
        ],
        QueryIntent::Context { .. } => vec![
            ClosureStrategy::CallGraph {
                direction: Direction::Both,
                depth: 1,
            },
            ClosureStrategy::TypeGraph {
                max_depth: type_depth,
            },
            import,
        ],
        QueryIntent::Explore { .. } => vec![
            ClosureStrategy::CallGraph {
                direction: Direction::Both,
                depth: 1,
            },
            import,
        ],
        QueryIntent::Path { .. } => vec![
            ClosureStrategy::CallGraph {
                direction: Direction::Both,
                depth: 1,
            },
            import,
        ],
        QueryIntent::Impact { .. } => vec![
            ClosureStrategy::CallGraph {
                direction: Direction::Both,
                depth: 1,
            },
            ClosureStrategy::TypeGraph {
                max_depth: type_depth,
            },
            import,
        ],
        QueryIntent::Search { .. } => vec![import, ClosureStrategy::SameDirectory],
        QueryIntent::TracePoint { .. } => vec![import, ClosureStrategy::SameDirectory],
        QueryIntent::TraceVariable { .. } if language == Language::ArkTS => vec![
            ClosureStrategy::CallGraph {
                direction: Direction::Both,
                depth: 1,
            },
            ClosureStrategy::TypeGraph {
                max_depth: type_depth,
            },
            import,
            ClosureStrategy::SameDirectory,
            ClosureStrategy::StateChannel,
        ],
        QueryIntent::TraceVariable { .. } => vec![
            ClosureStrategy::CallGraph {
                direction: Direction::Both,
                depth: 1,
            },
            ClosureStrategy::TypeGraph {
                max_depth: type_depth,
            },
            import,
            ClosureStrategy::SameDirectory,
        ],
    }
}

fn iterations_for(intent: &QueryIntent, background: bool) -> u32 {
    if !background {
        return 0;
    }
    match intent {
        QueryIntent::SemanticFunction { .. } => 0,
        QueryIntent::Calls { depth, .. } => depth.unwrap_or(1).clamp(1, 5) as u32,
        QueryIntent::Path { max_depth, .. } => max_depth.unwrap_or(5).clamp(1, 10) as u32,
        QueryIntent::Impact { depth, .. } => depth.unwrap_or(3).clamp(1, 5) as u32,
        QueryIntent::Explore { .. } | QueryIntent::Context { .. } => 2,
        QueryIntent::TraceVariable { max_depth, .. } => (*max_depth).clamp(2, 5) as u32,
        QueryIntent::Search { .. }
        | QueryIntent::SymbolDetail { .. }
        | QueryIntent::TracePoint { .. } => 1,
    }
}

// ── AccessStrategy ───────────────────────────────────────────────────────────────

/// Whether the project has a full index or needs focus-driven analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessStrategy {
    /// Full cache exists — use existing resolution + graph code path.
    FullCache,
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
    pub access: AccessStrategy,
    /// For Focus mode: precision of the returned results.
    pub quality: Option<AnswerQuality>,
    /// For Focus mode: known gaps.
    pub gaps: Vec<KnownGap>,
    /// For Focus mode: pending closure IDs being built in background.
    pub pending_closure_ids: Vec<String>,
    /// Raw extraction jobs encountered by foreground closure preparation.
    ///
    /// These come from `extraction_jobs` in-flight de-duplication. They are
    /// retryable pending work, but not Focus closure jobs and not terminal gaps.
    pub pending_extraction_job_ids: Vec<String>,
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

impl FocusResult {
    /// Pending work visible to the public retry model.
    pub fn pending_work_count_and_eta_ms(&self) -> (usize, u64) {
        let (closure_pending, closure_eta) = self
            .job_tracker
            .as_ref()
            .map(|tracker| tracker.pending_count_and_eta_ms(&self.pending_closure_ids))
            .unwrap_or((0, 0));
        let extraction_pending = self.pending_extraction_job_ids.len();
        let pending = closure_pending + extraction_pending;
        if pending == 0 {
            return (0, 0);
        }
        let extraction_eta = 5000 * extraction_pending as u64;
        (pending, (closure_eta + extraction_eta).clamp(5000, 60000))
    }

    /// Foreground and completed background files that must be reflected in the
    /// in-memory graph before replaying this query.
    pub fn materialized_files(&self) -> Vec<FileId> {
        let mut files = self.built_files.clone();
        if let Some(tracker) = &self.job_tracker {
            files.extend(tracker.built_files_for(&self.pending_closure_ids));
        }
        let mut seen = HashSet::new();
        files.retain(|file_id| seen.insert(*file_id));
        files
    }
}

// ── FocusRuntime ────────────────────────────────────────────────────────────

/// Single control-plane entry point for MCP Focus queries.
///
/// Detects access strategy, orchestrates bootstrap, seed location, closure
/// building, and background expansion. Materialize is Focus-owned
/// ([`FocusMaterialize`]).
pub struct FocusRuntime {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    bootstrap: BootstrapManager,
    scheduler: Arc<std::sync::Mutex<FocusScheduler>>,
    closure_engine: Option<ClosureEngine>,
    started: AtomicBool,
    /// Join handle for the background worker thread (if spawned).
    bg_handle: Option<JoinHandle<()>>,
    /// Index mode override for testing. When `Some`, `detect_access_strategy()`
    /// returns this value instead of calling `Store::read_catalog_tier()`.
    detect_access_strategy_override: Option<AccessStrategy>,
    /// Focus materialize stack (structural + dataflow with rebuilder).
    /// Required at construction — no silent second stack.
    materialize: FocusMaterialize,
    /// Runtime-owned hot region state. The scheduler executes jobs; this
    /// tracker decides whether a new query is extending an existing closure.
    hot_regions: HotRegionTracker,
    /// Shared tracker for background job completion.
    /// Foreground jobs are marked done immediately; background
    /// jobs are marked by the scheduler on completion.
    job_tracker: Arc<JobTracker>,
    /// Active or failed file warming job per source region, together with the
    /// incomplete seed that the job was required to materialize. Successful
    /// entries are replaced only when that seed completed and a later query
    /// still has incomplete files in the region. A completed job whose seed is
    /// still incomplete remains terminal so resume cannot enqueue it forever.
    file_focus_jobs: HashMap<String, (String, FileId)>,
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
        covered_depth: u32,
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

        region.depth = region.depth.max(covered_depth);

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

    fn reusable_jobs(&self, region_id: &str, tracker: &JobTracker) -> Vec<String> {
        self.regions
            .iter()
            .find(|region| region.id == region_id)
            .map(|region| {
                region
                    .pending_closure_ids
                    .iter()
                    .filter(|job_id| {
                        let ids = std::slice::from_ref(*job_id);
                        tracker.pending_count(ids) > 0 || !tracker.failures_for(ids).is_empty()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
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

    /// Create a new FocusRuntime with a required materialize stack.
    ///
    /// Does NOT start bootstrap. Callers (MCP `ActiveProject`, tests) must
    /// supply the same [`FocusMaterialize`] used by Engine / analysis ensure —
    /// there is no silent `FocusMaterialize::open` fallback on prepare.
    ///
    /// Detects whether the backing store is persistent (on-disk atlas.db)
    /// or in-memory, and configures the hot region tracker accordingly:
    /// persistent stores keep all hot regions indefinitely, while
    /// in-memory stores evict the shallowest/LRU region beyond
    /// [`MAX_MEMORY_HOT_REGIONS`].
    pub fn new(
        store: Arc<Store>,
        project_root: Option<PathBuf>,
        materialize: FocusMaterialize,
    ) -> Self {
        let is_persistent = store.db_path().to_string_lossy() != ":memory:";
        let job_tracker = Arc::new(JobTracker::new());
        Self {
            store: store.clone(),
            project_root: project_root.clone(),
            bootstrap: BootstrapManager::new(store.clone(), project_root.clone()),
            scheduler: Arc::new(std::sync::Mutex::new(
                FocusScheduler::new(store).with_job_tracker(Arc::clone(&job_tracker)),
            )),
            closure_engine: None,
            started: AtomicBool::new(false),
            bg_handle: None,
            detect_access_strategy_override: None,
            materialize,
            hot_regions: HotRegionTracker {
                is_persistent,
                ..HotRegionTracker::default()
            },
            job_tracker,
            file_focus_jobs: HashMap::new(),
        }
    }

    /// Focus materialize stack (always present after construction).
    pub fn materialize(&self) -> &FocusMaterialize {
        &self.materialize
    }

    // ── Index mode detection ─────────────────────────────────────────────

    /// Detect whether a CLI-finalized whole-project cache satisfies `need`, or
    /// whether the query must continue through incremental Focus.
    ///
    /// Focus can write rich layers for a small closure. Those rows are useful
    /// cache entries, but they are not proof that the repository is fully
    /// indexed. A full-index decision therefore requires both fresh rich
    /// extraction state, index-finalization metadata, whole-project scope, and
    /// the fact layer required by the current query.
    pub fn detect_access_strategy(&self, need: QueryNeed) -> AccessStrategy {
        if let Some(mode) = self.detect_access_strategy_override {
            return mode;
        }
        if crate::has_finalized_repo_cache_for(&self.store, need) {
            AccessStrategy::FullCache
        } else {
            AccessStrategy::Focus
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
    pub fn prepare(
        &mut self,
        intent: &QueryIntent,
        include_roots: Vec<IncludeRoot>,
    ) -> Result<FocusResult> {
        let mode = self.detect_access_strategy(intent.required_analysis());

        if mode == AccessStrategy::FullCache {
            return Ok(FocusResult {
                access: AccessStrategy::FullCache,
                quality: None,
                gaps: Vec::new(),
                pending_closure_ids: Vec::new(),
                pending_extraction_job_ids: Vec::new(),
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
        let minimal_iterations = iterations_for(intent, false);
        let minimal_budget = WindowBudget {
            max_iterations: minimal_iterations,
            ..WindowBudget::default()
        };
        let minimal_window = FocusWindow {
            seed: seed.clone(),
            strategies: strategies_for(intent, false, language),
            include_roots: include_roots.clone(),
            budget: minimal_budget,
            language,
            max_iterations: minimal_iterations,
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

        let mut gaps = closure.gaps.clone();
        if !bootstrap_ready {
            gaps.push(KnownGap::BudgetExhausted {
                strategy: "bootstrap_tier0_wait".to_string(),
                remaining: 0,
            });
        }

        // 5b. Derive precision from actual structural closure and gaps. A
        // resolution-symbol dependency is useful coverage, but not proof that
        // the requested code closure is structurally complete.
        let precision = if closure.files.is_empty() {
            AnswerQuality {
                coverage: CoverageTier::Boundary {
                    target_tier: SymbolTier::Manifest,
                },
                confidence: SemanticConfidence::Low,
            }
        } else if !gaps.is_empty() {
            AnswerQuality {
                coverage: CoverageTier::Partial { gaps: gaps.clone() },
                confidence: SemanticConfidence::Medium,
            }
        } else {
            AnswerQuality {
                coverage: CoverageTier::ClosureComplete {
                    closure_id: closure_id.clone(),
                },
                confidence: SemanticConfidence::High,
            }
        };

        let built_files: Vec<FileId> = closure.files.iter().copied().collect();
        if boundary_hit.is_none() {
            boundary_hit = self.hot_regions.boundary_hit_for_files(&built_files);
        }

        // Mark foreground closure as immediately done in the tracker.
        self.job_tracker.mark_done(&closure_id);
        let pending_closure_ids: Vec<String> = Vec::new();

        // 6. Enqueue background expansion
        let background_iterations = iterations_for(intent, true);
        let background_budget = WindowBudget {
            max_iterations: background_iterations,
            ..WindowBudget::background()
        };
        let bg_window = FocusWindow {
            seed,
            strategies: strategies_for(intent, true, language),
            include_roots: include_roots.clone(),
            budget: background_budget,
            language,
            max_iterations: background_iterations,
        };
        let mut pending_ids = pending_closure_ids;
        let needs_dataflow = intent.required_analysis() == QueryNeed::Dataflow
            && !matches!(
                intent,
                QueryIntent::SemanticFunction { .. }
                    if !matches!(language, Language::C | Language::Cpp)
            );
        if background_iterations > 0 || needs_dataflow {
            let priority = if needs_dataflow {
                FocusPriority::Sync
            } else {
                FocusPriority::UserFocus
            };
            // A structural hot-region job cannot satisfy a dataflow query.
            // Dataflow intents therefore enqueue their own Sync-grade closure;
            // resume reuses that exact tracked job through the query snapshot.
            if needs_dataflow {
                let closure_id = self.scheduler.lock().unwrap().enqueue(bg_window, priority);
                pending_ids.push(closure_id);
            } else if let Some(hit) = boundary_hit.as_ref() {
                let reusable = self
                    .hot_regions
                    .reusable_jobs(&hit.region_id, &self.job_tracker);
                if !reusable.is_empty() {
                    pending_ids.extend(reusable);
                } else if hit.depth < background_iterations {
                    let closure_id = self.scheduler.lock().unwrap().enqueue(bg_window, priority);
                    pending_ids.push(closure_id);
                }
            } else {
                let closure_id = self.scheduler.lock().unwrap().enqueue(bg_window, priority);
                pending_ids.push(closure_id);
            }
        }

        self.hot_regions.observe_closure(
            seed_file_id,
            &built_files,
            &pending_ids,
            background_iterations,
        );

        Ok(FocusResult {
            access: AccessStrategy::Focus,
            quality: Some(precision),
            gaps,
            pending_closure_ids: pending_ids,
            pending_extraction_job_ids: closure.pending_extraction_job_ids.clone(),
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

    /// Take files materialized by background jobs since the previous drain.
    ///
    /// This is the project-wide refresh feed. It is independent of any single
    /// query snapshot so both fresh requests and resume replays see completed
    /// background writes.
    pub fn take_background_refresh_files(&self) -> Vec<FileId> {
        self.job_tracker.take_refresh_files()
    }

    /// Enqueue file-focused background warming without building a foreground closure.
    ///
    /// Search uses this after its first bounded pass. MCP's outer 18-second gate
    /// withholds that provisional body while the tracked scheduler work runs,
    /// then either replays a complete response or returns a ticket.
    pub fn enqueue_file_focus_warm(&mut self, file_ids: &[FileId]) -> Result<Option<FocusResult>> {
        if file_ids.is_empty()
            || self.detect_access_strategy(QueryNeed::Structural) == AccessStrategy::FullCache
        {
            return Ok(None);
        }

        self.ensure_closure_engine()?;
        self.ensure_started();

        let mut groups: HashMap<String, Vec<FileId>> = HashMap::new();
        for file_id in file_ids.iter().filter(|file_id| {
            !self
                .materialize
                .structural()
                .has_structural_layer(file_id)
                .unwrap_or(false)
        }) {
            let key = self.file_focus_group_key(file_id);
            groups.entry(key).or_default().push(*file_id);
        }

        if groups.is_empty() {
            return Ok(None);
        }

        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|left, right| left.0.cmp(&right.0));
        let mut job_ids = Vec::with_capacity(groups.len());
        let mut scheduler = self.scheduler.lock().unwrap();
        for (group_key, group_files) in groups {
            if let Some((job_id, seed_file_id)) = self.file_focus_jobs.get(&group_key).cloned() {
                let ids = std::slice::from_ref(&job_id);
                if self.job_tracker.pending_count(ids) > 0
                    || !self.job_tracker.failures_for(ids).is_empty()
                {
                    job_ids.push(job_id);
                    continue;
                }
                let seed_complete = self
                    .materialize
                    .structural()
                    .has_structural_layer(&seed_file_id)
                    .unwrap_or(false);
                if !seed_complete {
                    // The completed window made no progress on its required
                    // seed. Re-enqueuing the same region would create an
                    // unbounded ready -> resume -> retry cycle. Reuse the
                    // terminal job so the caller publishes a bounded gap.
                    job_ids.push(job_id);
                    continue;
                }
                self.file_focus_jobs.remove(&group_key);
            }
            let file_id = group_files[0];
            let language = self.resolve_language_for_file(&file_id).unwrap_or_default();
            let window = FocusWindow {
                seed: FocusSeed::File { file_id, language },
                strategies: vec![
                    ClosureStrategy::ImportNeighborhood { depth: 1 },
                    ClosureStrategy::SameDirectory,
                ],
                include_roots: Vec::new(),
                budget: WindowBudget::background(),
                language,
                max_iterations: 2,
            };
            let job_id = scheduler.enqueue(window, FocusPriority::UserFocus);
            self.file_focus_jobs
                .insert(group_key, (job_id.clone(), file_id));
            job_ids.push(job_id);
        }
        drop(scheduler);

        Ok(Some(FocusResult {
            access: AccessStrategy::Focus,
            quality: None,
            gaps: Vec::new(),
            pending_closure_ids: job_ids,
            pending_extraction_job_ids: Vec::new(),
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: Vec::new(),
            coverage_counts: None,
            job_tracker: Some(Arc::clone(&self.job_tracker)),
        }))
    }

    fn file_focus_group_key(&self, file_id: &FileId) -> String {
        let registered_path = self
            .store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|file| file.path);
        if let Some(path) = registered_path {
            return Path::new(&path)
                .parent()
                .map(|parent| format!("dir:{}", parent.to_string_lossy().replace('\\', "/")))
                .unwrap_or_else(|| "dir:.".to_string());
        }

        // SameDirectory expands from registered `files`. Inventory-only
        // candidates therefore need their own seed until materialized; grouping
        // them by directory would silently discard every file but the first.
        self.store
            .find_file_inventory_by_id(file_id)
            .ok()
            .flatten()
            .map(|row| format!("inventory:{}", row.path))
            .unwrap_or_else(|| format!("file:{}", file_id.to_hex()))
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
            QueryIntent::SemanticFunction {
                symbol_name,
                file_id,
                symbol_id,
            } => self.locate_calls_seed(symbol_name, file_id, symbol_id),
            QueryIntent::SymbolDetail {
                symbol_name,
                file_id,
                symbol_id,
            } => self.locate_calls_seed(symbol_name, file_id, symbol_id),
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
                    file_id: None,
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
                ..
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
                .with_context(|| format!("Symbol not found: {sym_id:?}"))?;
            let seed = FocusSeed::Symbol {
                name: sym.name.clone(),
                kind: Some(sym.kind),
                language: sym.language,
                file_id: Some(sym.file_id),
            };
            return Ok((seed, Some(sym.file_id), Some(*sym_id), sym.language));
        }

        if let Some(fid) = file_id {
            let language = self.resolve_language_for_file(fid).unwrap_or_default();
            let seed = FocusSeed::Symbol {
                name: symbol_name.to_string(),
                kind: None,
                language,
                file_id: Some(*fid),
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
            file_id: seed_file_id,
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
        // Share Arc materialize with foreground + scheduler ClosureEngines.
        let engine = ClosureEngine::new(self.store.clone(), self.materialize.clone());
        let sched_engine = ClosureEngine::new(self.store.clone(), self.materialize.clone());

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

/// Integration test for request-scoped `include_roots` wiring.
///
/// Proves end-to-end through `FocusRuntime::prepare`:
/// 1. A query that supplies `include_roots` resolves an angle-bracket
///    `#include <net/dst.h>` to a project header (`include/net/dst.h`) and
///    materialises the header's resolution symbols into the closure coverage
///    (visible as the `"boundary"` tier in `coverage_counts`).
/// 2. A follow-up query on the same `FocusRuntime` (cached engine) that carries
///    NO roots does NOT resolve the header — proving per-query roots do not
///    leak across queries on reused focus runtime state.
#[cfg(test)]
mod include_roots_integration {
    use super::{AccessStrategy, FocusRuntime};
    use crate::closure_planner::IncludeRoot;
    use crate::focus::query::QueryIntent;
    use db::Store;
    use std::sync::Arc;
    use types::enums::{Language, ParseStatus};
    use types::ids::{FileId, ImportId};
    use types::structs::{FactCoverage, FileInfo, ImportDef};
    use types::{ImportKind, layer, status};

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    /// Insert a file and mark its structural layer complete so the focus
    /// closure treats it as already extracted (no disk read needed). A
    /// structurally-complete file also satisfies `has_resolution_symbols_layer`,
    /// so `materialize_import_dependencies` records it as a cached
    /// resolution-symbol dependency.
    fn insert_file_structural_complete(store: &Store, path: &str) -> FileId {
        let file_id = FileId::generate(path);
        store
            .upsert_file(&FileInfo {
                file_id,
                path: path.to_string(),
                language: Language::C,
                content_hash: "abc123".to_string(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                layer::STRUCTURAL,
                "abc123",
                status::COMPLETE,
                FactCoverage::default(),
            )
            .unwrap();
        file_id
    }

    /// Insert an angle-bracket `#include <net/dst.h>` record on `seed`.
    fn insert_angle_include(store: &Store, seed: FileId) {
        let import_id = ImportId::generate(&seed, "include", "net/dst.h", None, 0);
        store
            .insert_imports(&[ImportDef {
                id: import_id,
                file_id: seed,
                kind: ImportKind::Include,
                module: "net/dst.h".to_string(),
                imported_name: String::new(),
                local_name: None,
                alias: None,
                is_wildcard: false,
                is_relative: false, // angle-bracket include
                range: types::structs::TextRange::default(),
            }])
            .unwrap();
    }

    fn test_runtime_focus_mode(store: Arc<Store>) -> FocusRuntime {
        let m = crate::FocusMaterialize::open(store.clone(), None);
        let mut rt = FocusRuntime::new(store, None, m);
        rt.detect_access_strategy_override = Some(AccessStrategy::Focus);
        rt
    }

    #[test]
    fn include_roots_resolve_angle_include_and_do_not_leak_across_queries() {
        let store = test_store();
        let seed_id = insert_file_structural_complete(&store, "src/main.c");
        let header_id = insert_file_structural_complete(&store, "include/net/dst.h");
        insert_angle_include(&store, seed_id);

        let mut rt = test_runtime_focus_mode(store.clone());

        let intent = QueryIntent::Context {
            symbol_name: "main".to_string(),
            file_id: Some(seed_id),
            symbol_id: None,
        };

        // ── Query 1: WITH include_roots ──────────────────────────────────
        // `#include <net/dst.h>` should resolve via the "include" root to
        // `include/net/dst.h`, so the header is materialised as resolution
        // symbols and recorded in closure coverage under the
        // "extracted_resolution_symbols" source (mapped to "boundary" tier).
        let q1 = rt
            .prepare(
                &intent,
                vec![IncludeRoot {
                    path: "include".to_string(),
                }],
            )
            .expect("prepare (q1) must succeed");
        assert_eq!(q1.access, AccessStrategy::Focus);
        let q1_counts: std::collections::HashMap<String, usize> =
            q1.coverage_counts.clone().unwrap_or_default();
        assert!(
            q1_counts.contains_key("boundary"),
            "q1 (with include_roots): expected a 'boundary' coverage entry from the resolved \
             angle-include header, got: {q1_counts:?}"
        );
        // Confirm the header specifically was recorded as a resolved dependency
        // (source = "extracted_resolution_symbols" after visibility commit).
        let closure_id = q1.closure_id.as_ref().expect("q1 closure_id");
        let cov_rows = store.get_visible_coverage(closure_id).unwrap_or_default();
        let header_bytes = header_id.as_bytes().to_vec();
        assert!(
            cov_rows
                .iter()
                .any(|r| r.file_id == header_bytes && r.source == "extracted_resolution_symbols"),
            "q1: the resolved header (include/net/dst.h) must appear in closure coverage as an \
             extracted_resolution_symbols entry, got rows: {:?}",
            cov_rows
                .iter()
                .map(|r| (r.source.clone(), r.file_id.len()))
                .collect::<Vec<_>>()
        );

        // ── Query 2: NO include_roots (leak check) ───────────────────────
        // On the SAME FocusRuntime (cached engine), a follow-up query with no
        // roots must NOT resolve the angle include. If roots leaked from q1,
        // the header would still be resolved here.
        let q2 = rt
            .prepare(&intent, Vec::new())
            .expect("prepare (q2) must succeed");
        assert_eq!(q2.access, AccessStrategy::Focus);
        let q2_closure_id = q2.closure_id.as_ref().expect("q2 closure_id");
        let q2_cov_rows = store
            .get_visible_coverage(q2_closure_id)
            .unwrap_or_default();
        assert!(
            !q2_cov_rows.iter().any(|r| {
                r.file_id == header_bytes && r.source == "extracted_resolution_symbols"
            }),
            "q2 (no include_roots): the header must NOT be resolved into q2's closure — \
             per-query roots must not leak across queries on the cached engine."
        );
        let q2_counts: std::collections::HashMap<String, usize> =
            q2.coverage_counts.clone().unwrap_or_default();
        assert!(
            !q2_counts.contains_key("boundary"),
            "q2 (no include_roots): 'boundary' coverage must be absent (no leaked roots), got: {q2_counts:?}"
        );
    }
}
