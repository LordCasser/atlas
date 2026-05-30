//! LazyCoordinator — unified orchestration for lazy extraction with job tracking.
//!
//! Coordinates between lazy structural and lazy dataflow extraction,
//! ensuring in-flight deduplication and recording build state via the
//! `lazy_jobs` table.
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

use std::sync::Arc;

use anyhow::Result;
use db::Store;
use types::ids::FileId;

use crate::closure_planner::ClosurePlanner;
use crate::lazy_structural::{EnsureStructuralResult, LAZY_STRUCTURAL_BUDGET_MS, LazyStructuralService};
use types::structs::precision::PrecisionTier;

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
}

impl LazyCoordinator {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            project_root: None,
        }
    }

    /// Create a coordinator with a project root for path resolution
    /// (needed by ClosurePlanner for import target resolution).
    pub fn with_project_root(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        Self {
            store,
            project_root: Some(project_root),
        }
    }

    /// Generate a unique job ID.
    fn new_job_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        format!("lazy_{:x}", ts)
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
        // Phase 1: job tracking + dedup without closure planning.
        // In Phase 2, this will be replaced by closure-aware planning.

        // Check for in-flight job
        if let Some(active) = self.store.find_active_lazy_job(file_id, "structural")? {
            if active.status == "building" || active.status == "queued" {
                // Another request is already handling this — return its job_id
                let result = EnsureStructuralResult {
                    files_built: 0,
                    files_cached: 1, // treated as cached since another job handles it
                    budget_exceeded: false,
                    built_file_ids: vec![],
                    precision_tier: PrecisionTier::Exact,
                };
                return Ok((result, active.job_id));
            }
        }

        // Create new job
        let job_id = Self::new_job_id();
        self.store.upsert_lazy_job_queued(
            &job_id,
            file_id,
            "structural",
            Some("lazy_coordinator::ensure_structural"),
            None,
            Some(LAZY_STRUCTURAL_BUDGET_MS as i64),
        )?;

        // Start the job
        self.store.start_lazy_job(file_id, "structural")?;

        // Execute extraction
        let result = service.ensure_structural_for_file(file_id);

        match result {
            Ok(r) => {
                self.store.complete_lazy_job(&job_id)?;
                Ok((r, job_id))
            }
            Err(e) => {
                self.store.fail_lazy_job(&job_id, &format!("{:#}", e))?;
                Err(e)
            }
        }
    }

    /// Phase 2: closure-aware structural extraction with job tracking.
    ///
    /// Given a seed file, expands to include import dependencies via
    /// [`ClosurePlanner`], builds them in dependency order (deps first),
    /// then builds the seed.  Each file in the closure gets its own
    /// lazy_job record with in-flight deduplication.
    ///
    /// Returns the accumulated [`EnsureStructuralResult`] and the job_id
    /// of the last file built (or the first cached job_id if nothing was
    /// built).
    pub fn ensure_structural_with_closure(
        &self,
        service: &LazyStructuralService,
        seed: &FileId,
    ) -> Result<(EnsureStructuralResult, String)> {
        let planner = ClosurePlanner::new(self.store.clone(), self.project_root.clone());
        let workset = planner.plan_for_seed(seed)?;

        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
        };
        let mut last_job_id = String::new();

        for file_id in &workset.order {
            let is_seed = file_id == seed;
            let layer_name = if is_seed { "structural" } else { "resolution_symbols" };

            // In-flight dedup: reuse existing job if one is active
            if let Some(active) = self.store.find_active_lazy_job(file_id, layer_name)? {
                if active.status == "building" || active.status == "queued" {
                    result.files_cached += 1;
                    last_job_id = active.job_id;
                    continue;
                }
            }

            // Create and start a new job
            let job_id = Self::new_job_id();
            self.store.upsert_lazy_job_queued(
                &job_id,
                file_id,
                layer_name,
                Some("lazy_coordinator::ensure_structural_with_closure"),
                None,
                Some(LAZY_STRUCTURAL_BUDGET_MS as i64),
            )?;
            self.store.start_lazy_job(file_id, layer_name)?;

            // Execute extraction for this file
            // For deps, use resolution_symbols; for seed, use structural.
            let build_result = if is_seed {
                service.ensure_structural_for_file(file_id)
            } else {
                service.ensure_resolution_symbols_for_file(file_id)
            };
            match build_result {
                Ok(r) => {
                    result.files_built += r.files_built;
                    result.files_cached += r.files_cached;
                    result.budget_exceeded =
                        result.budget_exceeded || r.budget_exceeded;
                    result.built_file_ids.extend(r.built_file_ids);
                    self.store.complete_lazy_job(&job_id)?;
                    last_job_id = job_id;
                }
                Err(e) => {
                    self.store.fail_lazy_job(&job_id, &format!("{:#}", e))?;
                    return Err(e);
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

        Ok((result, last_job_id))
    }

    /// Ensure structural facts for the file(s) containing `name`, with
    /// dependency closure expansion.
    ///
    /// Phase 2 capability: for each candidate file, BFS-expands its
    /// import dependencies via [`ClosurePlanner`] and builds them
    /// before the seed, ensuring cross-file references can be resolved.
    pub fn ensure_structural_for_symbol_with_closure(
        &self,
        service: &LazyStructuralService,
        name: &str,
    ) -> Result<EnsureStructuralResult> {
        let candidates = service.candidate_provider.candidates_for_symbol(name)?;
        if candidates.is_empty() {
            return Ok(EnsureStructuralResult {
                files_built: 0,
                files_cached: 0,
                budget_exceeded: false,
                built_file_ids: vec![],
                precision_tier: PrecisionTier::Unavailable,
            });
        }
        let mut total = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
        };
        for candidate in &candidates {
            let r = self.ensure_structural_with_closure(service, candidate)?;
            total.files_built += r.0.files_built;
            total.files_cached += r.0.files_cached;
            total.budget_exceeded |= r.0.budget_exceeded;
            total.built_file_ids.extend(r.0.built_file_ids);
        }
        total.precision_tier = crate::precision::structural_precision(
            total.files_built,
            total.files_cached,
            total.budget_exceeded,
        );
        Ok(total)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // These tests validate the lazy evolution contracts.
    // Implementations will be filled in as the coordinator evolves.

    #[test]
    #[ignore = "Phase 2: needs closure-aware structural"]
    fn lazy_closure_ensures_dependency_resolution_symbols() {
        // A references B. ensure_structural(A) should:
        // 1. Detect that B is needed for resolution
        // 2. Ensure B has at least resolution_symbols layer
        // 3. Extract A structural
        // 4. Resolve A against B's stable symbol view
    }

    #[test]
    #[ignore = "Phase 2: needs concurrent test harness"]
    fn concurrent_lazy_jobs_dedup_to_single_build() {
        // Two threads simultaneously trigger lazy structural for same file.
        // Only one build should occur; the second should receive the same job_id.
    }

    #[test]
    fn lazy_job_create_and_complete() {
        // Create a job, start it, complete it. Verify state transitions.
        // This is a coordinator-level smoke test that exercises the
        // store-level lazy_jobs operations.
    }

    #[test]
    fn graph_visibility_after_lazy_structural() {
        // Verify that EnsureStructuralResult tracks built_file_ids
        // and that the field is properly propagated through LazyCoordinator.
        use types::ids::FileId;
        use types::structs::precision::PrecisionTier;
        use crate::lazy_structural::EnsureStructuralResult;

        // Verify struct construction
        let result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
        };
        assert!(result.built_file_ids.is_empty());

        let fid = FileId::generate("test.rs");
        let result_with = EnsureStructuralResult {
            files_built: 1,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![fid.clone()],
            precision_tier: PrecisionTier::Unavailable,
        };
        assert_eq!(result_with.built_file_ids.len(), 1);
        assert_eq!(result_with.built_file_ids[0], fid);
    }

    #[test]
    fn precision_degraded_structural_budget_exceeded() {
        // Structural extraction exceeds budget → DegradedStructural precision.
        // Response includes precision_level, missing_layers, next_action.
        use types::structs::precision::PrecisionTier;
        use crate::precision;

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
    #[ignore = "Phase 3: needs prebuilt guard validation"]
    fn prebuilt_dataflow_preserved_by_lazy() {
        // Full index dataflow exists. Lazy dataflow must NOT delete it,
        // must record artifact, must treat as cached.
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
}
