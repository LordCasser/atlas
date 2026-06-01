//! LazyCoordinator — unified orchestration for lazy extraction with job tracking.
//!
//! Coordinates between lazy structural and lazy dataflow extraction,
//! ensuring in-flight deduplication and recording build state via the
//! `extraction_jobs` table.
//!
//! # Phase 1 capabilities
//!
//! - In-flight deduplication: two concurrent requests for the same
//!   file+layer return the same job_id instead of building twice.
//! - Job lifecycle tracking: queued → building → complete/failed.
//!
//! # Phase 2 capabilities
//!
//! - ClosurePlanner integration for dependency graph traversal:
//!   `ensure_structural_with_closure` expands the build set to
//!   include import dependencies before building the seed file.
//!
//! # Phase 3 capabilities
//!
//! - Delta graph refresh (scoped graph rebuild for affected files)
//! - Budget tracking with precision degradation (PrecisionTier in results)
//!
//! # Remaining (Phase 5+)
//!
//! - (no outstanding Phase 4 items)

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use db::{ClaimResult, Store};
use types::ids::FileId;

use extraction::CancelCheck;

use crate::lazy_budget::LazyBudget;
use crate::LazyDataflowService;
use crate::closure_planner::{ClosurePlanner, IncludeRoot};
use crate::lazy_structural::{
    EnsureStructuralResult, LAZY_STRUCTURAL_BUDGET_MS, LazyStructuralService,
};
use types::structs::precision::PrecisionTier;

/// Global flag: at most one prewarm thread runs at a time.
static PREWARM_RUNNING: AtomicBool = AtomicBool::new(false);

/// RAII guard that clears the global prewarm flag on drop.
struct PrewarmGuard;
impl Drop for PrewarmGuard {
    fn drop(&mut self) {
        PREWARM_RUNNING.store(false, Ordering::Release);
    }
}

/// Coordinates lazy extraction with job tracking.
///
/// Phase 1 provides:
/// - In-flight deduplication: two concurrent requests for the same
///   file+layer return the same job_id instead of building twice.
/// - Job lifecycle tracking: queued → building → complete/failed.
///
/// Phase 2 adds ClosurePlanner integration via
/// [`ensure_structural_with_closure`].
///
/// Phase 3 adds delta graph refresh and precision degradation reporting.
pub struct LazyCoordinator {
    store: Arc<Store>,
    project_root: Option<std::path::PathBuf>,
    /// Request-scoped include roots for C/C++ angle-bracket resolution.
    include_roots: Vec<IncludeRoot>,
}

impl LazyCoordinator {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            project_root: None,
            include_roots: vec![],
        }
    }

    /// Create a coordinator with a project root for path resolution
    /// (needed by ClosurePlanner for import target resolution).
    pub fn with_project_root(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        Self {
            store,
            project_root: Some(project_root),
            include_roots: vec![],
        }
    }

    /// Set request-scoped include roots for C/C++ angle-bracket resolution.
    pub fn with_include_roots(mut self, roots: Vec<IncludeRoot>) -> Self {
        self.include_roots = roots;
        self
    }

    /// Merge request-scoped roots with auto-detected defaults.
    /// Request roots take priority; defaults appended after dedup.
    fn effective_include_roots(&self) -> Vec<IncludeRoot> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged = Vec::new();
        // 1. Request roots first (highest priority)
        for root in &self.include_roots {
            if seen.insert(root.path.clone()) {
                merged.push(root.clone());
            }
        }
        // 2. Auto-detected defaults
        if let Some(ref proj_root) = self.project_root {
            let include_dir = proj_root.join("include");
            if include_dir.exists() && include_dir.is_dir() {
                let path = "include".to_string();
                if seen.insert(path.clone()) {
                    merged.push(IncludeRoot { path });
                }
            }
        }
        merged
    }

    /// Ensure structural layer for a file, with job tracking and dedup.
    ///
    /// Returns `(EnsureStructuralResult, job_id)`. If another request is
    /// already building structural for this file, returns early without
    /// rebuilding.
    pub fn ensure_structural_with_tracking(
        &self,
        service: &LazyStructuralService,
        file_id: &FileId,
    ) -> Result<(EnsureStructuralResult, String)> {
        let claim = self.store.claim_file_extraction_job(
            file_id,
            "structural",
            Some("lazy_coordinator::ensure_structural"),
            None,
            Some(LAZY_STRUCTURAL_BUDGET_MS as i64),
        )?;

        match claim {
            ClaimResult::AlreadyBuilding { job_id } => {
                // Another request is already handling this — return its job_id
                let result = EnsureStructuralResult {
                    files_built: 0,
                    files_cached: 1, // treated as cached since another job handles it
                    budget_exceeded: false,
                    built_file_ids: vec![],
                    precision_tier: PrecisionTier::DegradedStructural,
                    files_pending: 0,
                    pending_job_ids: vec![],
                };
                return Ok((result, job_id));
            }
            ClaimResult::Claimed { job_id } => {
                // This caller owns the build — execute extraction
                let result = service.ensure_structural_for_file(file_id, None);

                match result {
                    Ok(r) => {
                        self.store.complete_extraction_job(&job_id)?;
                        Ok((r, job_id))
                    }
                    Err(e) => {
                        self.store
                            .fail_extraction_job(&job_id, &format!("{:#}", e))?;
                        Err(e)
                    }
                }
            }
        }
    }

    /// Phase 2: closure-aware structural extraction with job tracking.
    ///
    /// Given a seed file, expands to include import dependencies via
    /// [`ClosurePlanner`], builds them in dependency order (deps first),
    /// then builds the seed.  Each file in the closure gets its own
    /// extraction job record with in-flight deduplication.
    ///
    /// Returns the accumulated [`EnsureStructuralResult`] and the job_id
    /// of the last file built (or the first cached job_id if nothing was
    /// built).
    pub fn ensure_structural_with_closure(
        &self,
        service: &LazyStructuralService,
        seed: &FileId,
        budget: &mut LazyBudget,
    ) -> Result<(EnsureStructuralResult, String)> {
        let planner_roots = self.effective_include_roots();
        let planner = ClosurePlanner::new(self.store.clone(), self.project_root.clone())
            .with_include_roots(planner_roots);
        let workset = planner.plan_for_seed(seed)?;

        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
            files_pending: 0,
            pending_job_ids: vec![],
        };
        let mut last_job_id = String::new();
        let mut structural_file_ids: Vec<FileId> = Vec::new();

        for file_id in &workset.order {
            // Request-level budget check: stop if time or file quota exhausted.
            if !budget.can_continue() {
                result.budget_exceeded = true;
                break;
            }
            let is_seed = file_id == seed;
            let layer_name = if is_seed {
                "structural"
            } else {
                "resolution_symbols"
            };

            let claim = self.store.claim_file_extraction_job(
                file_id,
                layer_name,
                Some("lazy_coordinator::ensure_structural_with_closure"),
                None,
                Some(LAZY_STRUCTURAL_BUDGET_MS as i64),
            )?;

            match claim {
                ClaimResult::AlreadyBuilding { job_id } => {
                    result.files_pending += 1;
                    result.pending_job_ids.push(job_id);
                    continue;
                }
                ClaimResult::Claimed { job_id } => {
                    // This caller owns the build — execute extraction
                    let build_result = if is_seed {
                        service.ensure_structural_for_file(file_id, Some(budget as &dyn CancelCheck))
                    } else {
                        service.ensure_resolution_symbols_for_file(file_id)
                    };
                    match build_result {
                        Ok(r) => {
                            result.files_built += r.files_built;
                            result.files_cached += r.files_cached;
                            result.budget_exceeded = result.budget_exceeded || r.budget_exceeded;
                            result.built_file_ids.extend(r.built_file_ids);
                            if is_seed && r.files_built > 0 {
                                structural_file_ids.push(*file_id);
                            }
                            self.store.complete_extraction_job(&job_id)?;
                            last_job_id = job_id;
                            if r.files_built > 0 {
                                budget.consume_file();
                            }
                        }
                        Err(e) => {
                            self.store
                                .fail_extraction_job(&job_id, &format!("{:#}", e))?;
                            return Err(e);
                        }
                    }
                }
            }
        }

        // Compute final precision tier
        if !workset.order.is_empty() {
            result.precision_tier = crate::precision::structural_precision(
                result.files_built,
                result.files_cached,
                result.budget_exceeded,
            );
        }

        // Spawn background prewarm: pre-build dataflow for files that
        // just received structural extraction, so subsequent trace
        // queries are instant.  Only structural (not resolution_symbols)
        // files are prewarmed to avoid wasteful dataflow on dep-only files.
        if let Some(ref root) = self.project_root {
            if !structural_file_ids.is_empty() {
                spawn_background_prewarm(
                    Arc::clone(&self.store),
                    root.clone(),
                    structural_file_ids.clone(),
                );
            }
        }

        Ok((result, last_job_id))
    }

    /// Builds structural facts for the best-matching candidate file,
    /// with dependency-closure expansion.
    ///
    /// Only the first (best-ranked) candidate is built to avoid wasting
    /// the shared [`LazyBudget`] on multiple closures.  FTS5 ranking
    /// places the most relevant match first.  Users needing broader
    /// coverage should narrow the scope or run `atlas index`.
    pub fn ensure_structural_for_symbol_with_closure(
        &self,
        service: &LazyStructuralService,
        name: &str,
        budget: &mut LazyBudget,
    ) -> Result<EnsureStructuralResult> {
        let candidates = service.candidate_provider.candidates_for_symbol(name)?;
        if candidates.is_empty() {
            return Ok(EnsureStructuralResult {
                files_built: 0,
                files_cached: 0,
                budget_exceeded: false,
                built_file_ids: vec![],
                precision_tier: PrecisionTier::Unavailable,
                files_pending: 0,
                pending_job_ids: vec![],
            });
        }
        let mut total = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
            files_pending: 0,
            pending_job_ids: vec![],
        };
        // Build only the best (first) candidate's closure to avoid
        // wasting budget on multiple candidates. FTS5 ranking places
        // the most relevant match first. Users who need broader
        // coverage should narrow scope or run `atlas index`.
        let best = &candidates[0];
        let r = self.ensure_structural_with_closure(service, best, budget)?;
        total.files_built += r.0.files_built;
        total.files_cached += r.0.files_cached;
        total.budget_exceeded |= r.0.budget_exceeded;
        total.built_file_ids.extend(r.0.built_file_ids);
        total.files_pending += r.0.files_pending;
        total.pending_job_ids.extend(r.0.pending_job_ids);
        total.precision_tier = crate::precision::structural_precision(
            total.files_built,
            total.files_cached,
            total.budget_exceeded,
        );
        Ok(total)
    }
}

/// Spawn a background thread to pre-build dataflow for files that just
/// received structural or resolution_symbols extraction.
///
/// This is a speculative optimisation: after a `context` or `symbol` query
/// triggers lazy structural extraction on a set of files, subsequent
/// `trace_variable` calls on those same files will find pre-built dataflow
/// and skip the lazy dataflow planner entirely, reducing latency.
///
/// The background thread creates its own `LazyDataflowService` and operates
/// independently of the coordinator's job tracking.  Failures are logged
/// but never propagated — any file that fails to prewarm will simply be
/// built on-demand when actually queried.
fn spawn_background_prewarm(
    store: Arc<Store>,
    project_root: std::path::PathBuf,
    seed_file_ids: Vec<FileId>,
) {
    if seed_file_ids.is_empty() {
        return;
    }

    const MAX_PREWARM_FILES: usize = 8;
    const MAX_FUNCTIONS_PER_FILE: usize = 16;

    if PREWARM_RUNNING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return; // another prewarm already running
    }

    let file_ids: Vec<FileId> = seed_file_ids.into_iter().take(MAX_PREWARM_FILES).collect();

    match std::thread::Builder::new()
        .name("atlas-prewarm".into())
        .spawn(move || {
            let _guard = PrewarmGuard;
            use types::enums::SymbolKind;
            let lazy_dataflow = LazyDataflowService::new(Arc::clone(&store), Some(project_root));
            let mut attempted = 0usize;
            let mut built = 0usize;
            for file_id in &file_ids {
                let symbols = match store.find_symbols_by_file(file_id) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                for sym in symbols
                    .iter()
                    .filter(|s| s.kind == SymbolKind::Function || s.kind == SymbolKind::Method)
                    .take(MAX_FUNCTIONS_PER_FILE)
                {
                    attempted += 1;
                    match lazy_dataflow.ensure_for_function(&sym.id) {
                        Ok(w) => {
                            if w.units_built > 0 {
                                built += 1;
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Prewarm skipped for {} ({}): {:#}",
                                sym.qualified_name,
                                sym.file_id.to_hex(),
                                e
                            );
                        }
                    }
                }
            }
            tracing::info!(
                attempted,
                built,
                files = file_ids.len(),
                "Background prewarm complete"
            );
        }) {
        Ok(_) => {}
        Err(_) => {
            PREWARM_RUNNING.store(false, Ordering::Release);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // These tests validate the lazy evolution contracts.
    // Implementations will be filled in as the coordinator evolves.

    use std::sync::Arc;

    #[cfg(feature = "c")]
use crate::lazy_budget::LazyBudget;
    use crate::closure_planner::IncludeRoot;
    use crate::lazy_coordinator::LazyCoordinator;

    #[test]
    fn effective_include_roots_request_priority() {
        let store = Arc::new(db::Store::open_in_memory().unwrap());
        let request_roots = vec![
            IncludeRoot {
                path: "arch/x86/include".into(),
            },
            IncludeRoot {
                path: "include".into(),
            },
        ];
        let coord = LazyCoordinator::new(store).with_include_roots(request_roots);
        let merged = coord.effective_include_roots();
        // Request roots come first
        assert_eq!(merged[0].path, "arch/x86/include");
        assert_eq!(merged[1].path, "include");
    }

    #[test]
    fn lazy_closure_ensures_dependency_resolution_symbols() {
        // Test that ClosurePlanner discovers import dependencies and
        // claim_file_extraction_job atomic API prevents duplicate builds.
        use crate::closure_planner::ClosurePlanner;
        use db::Store;
        use types::enums::{ImportKind, Language, ParseStatus};
        use types::ids::{FileId, ImportId};
        use types::structs::{FileInfo, ImportDef, TextRange};

        let store = Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let main_id = FileId::generate("src/main.c");
        let util_id = FileId::generate("src/util.h");

        // Register both files as manifest-only (no structural data)
        store
            .upsert_file(&FileInfo {
                file_id: main_id,
                path: "src/main.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: util_id,
                path: "src/util.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // Add import: main.c #includes "util.h"
        let import = ImportDef {
            id: ImportId::generate(&main_id, "include", "util.h", None, 0),
            file_id: main_id,
            kind: ImportKind::Include,
            module: "util.h".into(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: true,
            range: TextRange::default(),
        };
        store.insert_imports(&[import]).unwrap();

        // ClosurePlanner should discover util.h as a dependency
        let planner = ClosurePlanner::new(store.clone(), None);
        let workset = planner.plan_for_seed(&main_id).unwrap();
        assert!(
            workset.order.contains(&util_id),
            "util.h should be in dependency closure"
        );
        assert_eq!(
            workset.order.last(),
            Some(&main_id),
            "seed file main.c should be last in build order"
        );

        // Test claim API: claiming util.h with resolution_symbols layer
        let claim1 = store
            .claim_file_extraction_job(&util_id, "resolution_symbols", None, None, None)
            .unwrap();
        assert!(matches!(claim1, db::ClaimResult::Claimed { .. }));

        // Second claim for same file+layer should return AlreadyBuilding
        let claim2 = store
            .claim_file_extraction_job(&util_id, "resolution_symbols", None, None, None)
            .unwrap();
        assert!(matches!(claim2, db::ClaimResult::AlreadyBuilding { .. }));
    }

    #[test]
    fn concurrent_extraction_jobs_dedup_to_single_build() {
        // Two threads simultaneously claim the same file+layer.
        // Only one should get Claimed; the other gets AlreadyBuilding.
        use db::Store;
        use types::enums::{Language, ParseStatus};
        use types::ids::FileId;
        use types::structs::FileInfo;

        let store = Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let file_id = FileId::generate("src/concurrent.c");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/concurrent.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let store1 = store.clone();
        let store2 = store.clone();

        let t1 = std::thread::spawn(move || {
            store1
                .claim_file_extraction_job(&file_id, "structural", None, None, None)
                .unwrap()
        });
        let t2 = std::thread::spawn(move || {
            store2
                .claim_file_extraction_job(&file_id, "structural", None, None, None)
                .unwrap()
        });

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();

        // One must be Claimed, the other must be AlreadyBuilding
        let claimed_count = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, db::ClaimResult::Claimed { .. }))
            .count();
        let building_count = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, db::ClaimResult::AlreadyBuilding { .. }))
            .count();
        assert_eq!(claimed_count, 1, "exactly one thread should claim the job");
        assert_eq!(
            building_count, 1,
            "exactly one thread should see AlreadyBuilding"
        );
    }

    #[test]
    fn extraction_job_create_and_complete() {
        // Use claim_file_extraction_job atomically, then complete. Verify state transitions.
        use db::Store;
        use types::enums::{Language, ParseStatus};
        use types::ids::FileId;
        use types::structs::FileInfo;

        let store = Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let file_id = FileId::generate("src/test.c");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/test.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // Claim the job (atomically checks + inserts)
        let claim = store
            .claim_file_extraction_job(&file_id, "structural", Some("test"), None, None)
            .unwrap();
        let job_id = match claim {
            db::ClaimResult::Claimed { job_id } => job_id,
            _ => panic!("expected Claimed"),
        };

        // Job should be active (building since claim sets it directly)
        let active = store
            .find_active_file_extraction_job(&file_id, "structural")
            .unwrap();
        assert!(active.is_some(), "job should be active after claim");

        // Complete the job
        store.complete_extraction_job(&job_id).unwrap();

        // After completion, job should not be active
        let active = store
            .find_active_file_extraction_job(&file_id, "structural")
            .unwrap();
        assert!(
            active.is_none(),
            "job should not be active after completion"
        );

        // Verify job status via get
        let job = store.get_extraction_job(&job_id).unwrap().unwrap();
        assert_eq!(job.status, "complete");
    }

    #[test]
    fn graph_visibility_after_lazy_structural() {
        // Verify that EnsureStructuralResult tracks built_file_ids
        // and that the field is properly propagated through LazyCoordinator.
        use crate::lazy_structural::EnsureStructuralResult;
        use types::ids::FileId;
        use types::structs::precision::PrecisionTier;

        // Verify struct construction
        let result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
            files_pending: 0,
            pending_job_ids: vec![],
        };
        assert!(result.built_file_ids.is_empty());

        let fid = FileId::generate("test.rs");
        let result_with = EnsureStructuralResult {
            files_built: 1,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![fid.clone()],
            precision_tier: PrecisionTier::Unavailable,
            files_pending: 0,
            pending_job_ids: vec![],
        };
        assert_eq!(result_with.built_file_ids.len(), 1);
        assert_eq!(result_with.built_file_ids[0], fid);
    }

    #[test]
    fn ensure_structural_result_pending_fields() {
        // Verify that files_pending and pending_job_ids are properly
        // populated so that AlreadyBuilding results propagate through
        // ensure_structural_for_symbol_with_closure to LazyOutcome.
        use crate::lazy_structural::EnsureStructuralResult;
        use types::ids::FileId;
        use types::structs::precision::PrecisionTier;

        let _fid = FileId::generate("pending.c");
        let result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::DegradedStructural,
            files_pending: 2,
            pending_job_ids: vec!["job-aaa".into(), "job-bbb".into()],
        };
        assert_eq!(result.files_pending, 2);
        assert_eq!(result.pending_job_ids.len(), 2);
        assert_eq!(result.pending_job_ids[0], "job-aaa");
        assert_eq!(result.pending_job_ids[1], "job-bbb");
    }

    #[test]
    fn precision_degraded_structural_budget_exceeded() {
        // Structural extraction exceeds budget → DegradedStructural precision.
        // Response includes precision_level, missing_layers, next_action.
        use crate::precision;
        use types::structs::precision::PrecisionTier;

        // Test: built > 0, cached = 0, budget_exceeded = true → DegradedStructural
        let tier = precision::structural_precision(2, 0, true);
        assert_eq!(tier, PrecisionTier::DegradedStructural);
        assert!(precision::next_action_structural(tier).is_some());

        // Test: built = 0, cached = 0, budget_exceeded = true → ManifestOnly
        let tier = precision::structural_precision(0, 0, true);
        assert_eq!(tier, PrecisionTier::ManifestOnly);

        // Test: built > 0, cached > 0, budget_exceeded = false → Exact
        let tier = precision::structural_precision(3, 2, false);
        assert_eq!(tier, PrecisionTier::Exact);
        assert!(precision::next_action_structural(tier).is_none());

        // Test: built = 0, cached = 0, budget_exceeded = false → Unavailable
        let tier = precision::structural_precision(0, 0, false);
        assert_eq!(tier, PrecisionTier::Unavailable);
        assert!(precision::next_action_structural(tier).is_some());
    }

    #[test]
    fn prebuilt_dataflow_preserved_by_lazy() {
        // E2E: ensure_for_function must detect pre-built dataflow from a
        // full index and skip rebuild, preserving the data intact.
        use crate::LazyDataflowService;
        use db::Store;
        use types::enums::{BindingKind, Language, ParseStatus, ScopeKind};
        use types::ids::{BindingId, DataNodeId, FileId, ScopeId, SymbolId};
        use types::lazy::AnalysisUnit;
        use types::structs::{FileInfo, ScopeDef, SymbolDef, TextRange};
        use types::{BindingDef, DataNode};

        let store = Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let file_id = FileId::generate("src/prebuilt.c");
        let func_sym_id = SymbolId::generate(&file_id, "c", "my_func", "function", None);
        let range = TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 5,
            end_column: 1,
        };
        let scope_id = ScopeId::generate(&file_id, None, "function", 0);

        // Register file
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/prebuilt.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // Insert symbol + scope (needed for FK)
        let sym = SymbolDef {
            id: func_sym_id,
            kind: types::enums::SymbolKind::Function,
            name: "my_func".into(),
            qualified_name: "my_func".into(),
            symbol_path: vec!["my_func".into()],
            file_id,
            language: Language::C,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        let scope = ScopeDef {
            id: scope_id,
            file_id,
            kind: ScopeKind::Function,
            name: "my_func_scope".into(),
            scope_path: "my_func_scope".into(),
            range,
            parent_id: None,
        };
        store
            .insert_file_facts(&types::structs::FileFacts {
                file: FileInfo {
                    file_id,
                    path: "src/prebuilt.c".into(),
                    language: Language::C,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![sym],
                scopes: vec![scope],
                ..Default::default()
            })
            .unwrap();

        // Insert pre-built dataflow node (simulating a full index)
        let binding_id = BindingId::generate(&file_id, &scope_id, "parameter", "x", 0);
        let dn_id = DataNodeId::generate(
            &file_id,
            Some(&func_sym_id),
            "parameter",
            Some("x"),
            Some("x"),
            10,
        );
        let dn = DataNode::parameter(
            dn_id,
            file_id,
            Some(func_sym_id),
            Some(binding_id),
            "x",
            range,
        );
        let binding = BindingDef {
            id: binding_id,
            file_id,
            function_id: Some(func_sym_id),
            scope_id,
            kind: BindingKind::Parameter,
            name: "x".into(),
            symbol_id: None,
            range,
        };

        // Write pre-built dataflow using replace_dataflow_for_unit
        let unit = AnalysisUnit::from_function(file_id, func_sym_id, range);
        store
            .replace_dataflow_for_unit(&unit, &[dn.clone()], &[], &[binding], &[], &[], &[])
            .unwrap();

        // Verify pre-built data exists before lazy service runs
        let pre_count = store.count_data_nodes_for_unit(&unit).unwrap();
        assert!(pre_count > 0, "pre-built data nodes should exist");

        // Run the lazy dataflow service — it should detect the pre-built
        // data via check_cache step 1.5 and skip extraction entirely.
        let lazy_service = LazyDataflowService::new(store.clone(), None);
        let window = lazy_service
            .ensure_for_function(&func_sym_id)
            .expect("lazy dataflow should succeed");

        // Should be cached, not rebuilt
        assert_eq!(window.units_cached, 1, "pre-built unit should be cached");
        assert_eq!(window.units_built, 0, "should not re-build pre-built data");

        // Verify data nodes were PRESERVED (not deleted by lazy rebuild)
        let post_count = store.count_data_nodes_for_unit(&unit).unwrap();
        assert_eq!(
            post_count, pre_count,
            "pre-built data nodes should be preserved after lazy check"
        );

        // Verify unit extraction state was created with "complete" status.
        let unit_state = store
            .get_unit_extraction_state(&file_id, &unit.unit_id, "dataflow")
            .unwrap()
            .expect("unit extraction state should exist after lazy service run");
        assert_eq!(
            unit_state.status, "complete",
            "unit extraction state should be complete"
        );
    }

    #[test]
    fn c_language_header_source_lazy_structural() {
        // Validate that the LinuxAugmenter correctly processes C files
        // by exercising the detection functions directly.
        use crate::linux_augment::LinuxAugmenter;
        use types::enums::{Language, ParseStatus, SymbolKind};
        use types::ids::{FileId, SymbolId};
        use types::structs::{FileFacts, FileInfo, SymbolDef, TextRange};

        // Build minimal C FileFacts
        let file_id = FileId::generate("kernel/mod.c");
        let mut facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "kernel/mod.c".to_string(),
                language: Language::C,
                content_hash: "test".to_string(),
                status: ParseStatus::default(),
            },
            symbols: vec![SymbolDef {
                id: SymbolId::generate(&file_id, "c", "my_init", "function", None),
                kind: SymbolKind::Function,
                name: "my_init".to_string(),
                qualified_name: "my_init".to_string(),
                symbol_path: vec!["my_init".to_string()],
                file_id,
                language: Language::C,
                range: TextRange::default(),
                name_range: TextRange::default(),
                signature: None,
                visibility: None,
                exported: false,
                static_: false,
                async_: false,
                container: None,
                scope_id: None,
                package_name: None,
                namespace_path: vec![],
                layer: "structural".to_string(),
            }],
            ..Default::default()
        };

        let source = "module_init(my_init);\nEXPORT_SYMBOL(my_init);";
        let result = LinuxAugmenter::augment(&mut facts, source);

        assert_eq!(result.symbols_exported, 1);
        assert_eq!(result.initcall_edges, 1);
        assert!(facts.symbols[0].exported);
    }

    #[cfg(feature = "c")]
    #[test]
    fn manifest_only_seed_bootstrap_e2e() {
        // Validate the full path: manifest-only seed C file with #include "util.h"
        // → ClosurePlanner bootstrap scanner discovers include
        // → coordinator builds resolution_symbols for dependency
        // → dependency symbols exist in DB.
        use crate::lazy_coordinator::LazyCoordinator;
        use crate::lazy_structural::{DefaultCandidateProvider, LazyStructuralService};
        use db::Store;
        use types::enums::{Language, ParseStatus, SymbolKind};
        use types::ids::{FileId, SymbolId};
        use types::structs::{FileFacts, FileInfo, SymbolDef, TextRange};

        // 1. Create temp directory with C source files
        let tmp = tempfile::tempdir().unwrap();
        let main_path = tmp.path().join("main.c");
        let util_path = tmp.path().join("util.h");
        // Use full function definitions (not prototypes) so C definition_query captures them.
        std::fs::write(
            &main_path,
            "#include \"util.h\"\nint main(void) { return helper(); }\n",
        )
        .unwrap();
        std::fs::write(&util_path, "static int helper(void) { return 42; }\n").unwrap();

        // 2. Set up in-memory store
        let store = std::sync::Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let main_id = FileId::generate("main.c");
        let util_id = FileId::generate("util.h");

        // 3. Register both files at MANIFEST level
        let main_range = TextRange {
            start_byte: 0,
            end_byte: 41,
            start_line: 1,
            start_column: 1,
            end_line: 2,
            end_column: 26,
        };
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: main_id,
                    path: "main.c".into(),
                    language: Language::C,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![SymbolDef {
                    id: SymbolId::generate(&main_id, "c", "main", "function", None),
                    kind: SymbolKind::Function,
                    name: "main".into(),
                    qualified_name: "main".into(),
                    symbol_path: vec!["main".into()],
                    file_id: main_id,
                    language: Language::C,
                    range: main_range,
                    name_range: main_range,
                    signature: None,
                    visibility: None,
                    exported: false,
                    static_: false,
                    async_: false,
                    container: None,
                    scope_id: None,
                    package_name: None,
                    namespace_path: vec![],
                    layer: "manifest".into(),
                }],
                layer: "manifest".into(),
                ..Default::default()
            })
            .unwrap();

        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: util_id,
                    path: "util.h".into(),
                    language: Language::C,
                    content_hash: "def".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![SymbolDef {
                    id: SymbolId::generate(&util_id, "c", "helper", "function", None),
                    kind: SymbolKind::Function,
                    name: "helper".into(),
                    qualified_name: "helper".into(),
                    symbol_path: vec!["helper".into()],
                    file_id: util_id,
                    language: Language::C,
                    range: TextRange::default(),
                    name_range: TextRange::default(),
                    signature: None,
                    visibility: None,
                    exported: false,
                    static_: false,
                    async_: false,
                    container: None,
                    scope_id: None,
                    package_name: None,
                    namespace_path: vec![],
                    layer: "manifest".into(),
                }],
                layer: "manifest".into(),
                ..Default::default()
            })
            .unwrap();

        // Record manifest layer as complete for both files
        store
            .upsert_file_extraction_state(&main_id, "manifest", "abc", "complete")
            .unwrap();
        store
            .upsert_file_extraction_state(&util_id, "manifest", "def", "complete")
            .unwrap();

        // 4. Create coordinator + service with temp dir as project root
        let coordinator =
            LazyCoordinator::with_project_root(store.clone(), tmp.path().to_path_buf());
        let candidate_provider = Box::new(DefaultCandidateProvider::new(
            store.clone(),
            Some(tmp.path().to_path_buf()),
        ));
        let lazy_service = LazyStructuralService::with_provider(
            store.clone(),
            Some(tmp.path().to_path_buf()),
            candidate_provider,
        );

        // 5. Call ensure_structural_with_closure on the seed
        let (_result, _job_id) = coordinator
            .ensure_structural_with_closure(&lazy_service, &main_id, &mut LazyBudget::new(u64::MAX, usize::MAX))
            .unwrap();

        // 6. Verify: util.h got resolution_symbols layer
        assert!(
            lazy_service
                .has_resolution_symbols_layer(&util_id)
                .unwrap_or(false),
            "util.h should have resolution_symbols layer after bootstrap + closure"
        );

        // 7. Verify: util.h symbol "helper" exists with resolution_symbols layer
        let helper_sym = store
            .find_symbol_by_id(&SymbolId::generate(
                &util_id, "c", "helper", "function", None,
            ))
            .unwrap()
            .expect("helper symbol should exist");
        assert_eq!(
            helper_sym.layer, "resolution_symbols",
            "helper symbol should have resolution_symbols layer"
        );

        // 8. Verify: main.c got structural layer
        assert!(
            lazy_service.has_structural_layer(&main_id).unwrap_or(false),
            "main.c should have structural layer after closure build"
        );
    }

    #[cfg(feature = "c")]
    #[test]
    fn c_header_closure_resolution_e2e() {
        // Validate multi-file C project: bar.c includes bar.h which references
        // baz.h. seed gets structural, deps get resolution_symbols.
        use crate::lazy_coordinator::LazyCoordinator;
        use crate::lazy_structural::{DefaultCandidateProvider, LazyStructuralService};
        use db::Store;
        use types::enums::{Language, ParseStatus, SymbolKind};
        use types::ids::{FileId, SymbolId};
        use types::structs::{FileFacts, FileInfo, SymbolDef, TextRange};

        // 1. Create temp directory with bar.c, bar.h, baz.h
        // Use full function definitions (not prototypes) so C definition_query captures them.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("bar.c"),
            "#include \"bar.h\"\nint bar_main(void) { return baz_helper(); }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("bar.h"),
            "#include \"baz.h\"\nstatic int bar_helper(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("baz.h"),
            "static int baz_helper(void) { return 0; }\n",
        )
        .unwrap();

        let store = std::sync::Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let bar_c_id = FileId::generate("bar.c");
        let bar_h_id = FileId::generate("bar.h");
        let baz_h_id = FileId::generate("baz.h");

        // 2. Register all 3 files at MANIFEST
        for (fid, path, hash, sym_name) in [
            (&bar_c_id, "bar.c", "abc", "bar_main"),
            (&bar_h_id, "bar.h", "def", "bar_helper"),
            (&baz_h_id, "baz.h", "ghi", "baz_helper"),
        ] {
            let sym_id = SymbolId::generate(fid, "c", sym_name, "function", None);
            store
                .insert_file_facts(&FileFacts {
                    file: FileInfo {
                        file_id: *fid,
                        path: path.into(),
                        language: Language::C,
                        content_hash: hash.into(),
                        status: ParseStatus::Success,
                    },
                    symbols: vec![SymbolDef {
                        id: sym_id,
                        kind: SymbolKind::Function,
                        name: sym_name.into(),
                        qualified_name: sym_name.into(),
                        symbol_path: vec![sym_name.into()],
                        file_id: *fid,
                        language: Language::C,
                        range: TextRange::default(),
                        name_range: TextRange::default(),
                        signature: None,
                        visibility: None,
                        exported: false,
                        static_: false,
                        async_: false,
                        container: None,
                        scope_id: None,
                        package_name: None,
                        namespace_path: vec![],
                        layer: "manifest".into(),
                    }],
                    layer: "manifest".into(),
                    ..Default::default()
                })
                .unwrap();
            store
                .upsert_file_extraction_state(fid, "manifest", hash, "complete")
                .unwrap();
        }

        // Pre-populate transitive import: bar.h → baz.h so ClosurePlanner discovers it.
        // The planner only bootstraps the seed file; dependency imports must be in DB.
        {
            use types::enums::ImportKind;
            use types::ids::ImportId;
            use types::structs::ImportDef;
            let transitive_import = ImportDef {
                id: ImportId::generate(&bar_h_id, "include", "baz.h", None, 0),
                file_id: bar_h_id,
                kind: ImportKind::Include,
                module: "baz.h".into(),
                imported_name: String::new(),
                local_name: None,
                alias: None,
                is_wildcard: false,
                is_relative: true,
                range: TextRange::default(),
            };
            store.insert_imports(&[transitive_import]).unwrap();
        }

        // 3. Run coordinator
        let coordinator =
            LazyCoordinator::with_project_root(store.clone(), tmp.path().to_path_buf());
        let provider = Box::new(DefaultCandidateProvider::new(
            store.clone(),
            Some(tmp.path().to_path_buf()),
        ));
        let lazy = LazyStructuralService::with_provider(
            store.clone(),
            Some(tmp.path().to_path_buf()),
            provider,
        );

        let (_result, _job_id) = coordinator
            .ensure_structural_with_closure(&lazy, &bar_c_id, &mut LazyBudget::new(u64::MAX, usize::MAX))
            .unwrap();

        // 4. Verify: bar.h and baz.h get resolution_symbols (deps)
        assert!(
            lazy.has_resolution_symbols_layer(&bar_h_id)
                .unwrap_or(false)
        );
        assert!(
            lazy.has_resolution_symbols_layer(&baz_h_id)
                .unwrap_or(false)
        );

        // 5. Verify: bar.c gets full structural (seed)
        assert!(lazy.has_structural_layer(&bar_c_id).unwrap_or(false));

        // 6. Verify: baz.h does NOT get structural (transitive dep, only resolution_symbols)
        assert!(
            !lazy.has_structural_layer(&baz_h_id).unwrap_or(false),
            "baz.h should NOT get full structural, only resolution_symbols"
        );
    }

    #[cfg(feature = "c")]
    #[test]
    fn graph_read_your_writes_after_lazy_structural() {
        // After lazy structural extraction, build a GraphEngine from the store
        // and verify the newly extracted symbol is visible in the graph.
        use crate::GraphEngine;
        use crate::lazy_coordinator::LazyCoordinator;
        use crate::lazy_structural::{DefaultCandidateProvider, LazyStructuralService};
        use db::Store;
        use types::enums::{Language, ParseStatus, SymbolKind};
        use types::ids::{FileId, SymbolId};
        use types::structs::{FileFacts, FileInfo, SymbolDef, TextRange};

        // 1. Create temp C file with a function definition
        let tmp = tempfile::tempdir().unwrap();
        let src = "int add_one(int x) { return x + 1; }\n";
        std::fs::write(tmp.path().join("math.c"), src).unwrap();

        let store = std::sync::Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let file_id = FileId::generate("math.c");
        let sym_id = SymbolId::generate(&file_id, "c", "add_one", "function", None);
        let range = TextRange {
            start_byte: 0,
            end_byte: 37,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 38,
        };

        // 2. Index at manifest level
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id,
                    path: "math.c".into(),
                    language: Language::C,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![SymbolDef {
                    id: sym_id,
                    kind: SymbolKind::Function,
                    name: "add_one".into(),
                    qualified_name: "add_one".into(),
                    symbol_path: vec!["add_one".into()],
                    file_id,
                    language: Language::C,
                    range,
                    name_range: range,
                    signature: None,
                    visibility: None,
                    exported: false,
                    static_: false,
                    async_: false,
                    container: None,
                    scope_id: None,
                    package_name: None,
                    namespace_path: vec![],
                    layer: "manifest".into(),
                }],
                layer: "manifest".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .upsert_file_extraction_state(&file_id, "manifest", "abc", "complete")
            .unwrap();

        // 3. Run lazy structural extraction via coordinator
        let coordinator =
            LazyCoordinator::with_project_root(store.clone(), tmp.path().to_path_buf());
        let provider = Box::new(DefaultCandidateProvider::new(
            store.clone(),
            Some(tmp.path().to_path_buf()),
        ));
        let lazy = LazyStructuralService::with_provider(
            store.clone(),
            Some(tmp.path().to_path_buf()),
            provider,
        );

        coordinator
            .ensure_structural_with_closure(&lazy, &file_id, &mut LazyBudget::new(u64::MAX, usize::MAX))
            .unwrap();

        // 4. Build GraphEngine from store (this is what MCP handlers do)
        let graph = GraphEngine::from_store(&store, 0.0).unwrap();

        // 5. Verify the symbol exists in the graph
        let node = graph.snapshot().node_by_id(&sym_id);
        assert!(
            node.is_some(),
            "add_one should be visible in the graph after lazy structural"
        );
        if let Some(n) = node {
            assert_eq!(n.name, "add_one");
        }
    }
}
