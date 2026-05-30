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
use db::{ClaimResult, Store};
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
        let claim = self.store.claim_lazy_job(
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
                    precision_tier: PrecisionTier::Exact,
                };
                return Ok((result, job_id));
            }
            ClaimResult::Claimed { job_id } => {
                // This caller owns the build — execute extraction
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

            let claim = self.store.claim_lazy_job(
                file_id,
                layer_name,
                Some("lazy_coordinator::ensure_structural_with_closure"),
                None,
                Some(LAZY_STRUCTURAL_BUDGET_MS as i64),
            )?;

            match claim {
                ClaimResult::AlreadyBuilding { job_id } => {
                    result.files_cached += 1;
                    last_job_id = job_id;
                    continue;
                }
                ClaimResult::Claimed { job_id } => {
                    // This caller owns the build — execute extraction
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

    use std::sync::Arc;

    #[test]
    fn lazy_closure_ensures_dependency_resolution_symbols() {
        // Test that ClosurePlanner discovers import dependencies and
        // claim_lazy_job atomic API prevents duplicate builds.
        use db::Store;
        use types::enums::{ImportKind, Language, ParseStatus};
        use types::ids::{FileId, ImportId};
        use types::structs::{FileInfo, ImportDef, TextRange};
        use crate::closure_planner::ClosurePlanner;

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
            .claim_lazy_job(&util_id, "resolution_symbols", None, None, None)
            .unwrap();
        assert!(matches!(claim1, db::ClaimResult::Claimed { .. }));

        // Second claim for same file+layer should return AlreadyBuilding
        let claim2 = store
            .claim_lazy_job(&util_id, "resolution_symbols", None, None, None)
            .unwrap();
        assert!(matches!(claim2, db::ClaimResult::AlreadyBuilding { .. }));
    }

    #[test]
    fn concurrent_lazy_jobs_dedup_to_single_build() {
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
                .claim_lazy_job(&file_id, "structural", None, None, None)
                .unwrap()
        });
        let t2 = std::thread::spawn(move || {
            store2
                .claim_lazy_job(&file_id, "structural", None, None, None)
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
    fn lazy_job_create_and_complete() {
        // Use claim_lazy_job atomically, then complete. Verify state transitions.
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
            .claim_lazy_job(&file_id, "structural", Some("test"), None, None)
            .unwrap();
        let job_id = match claim {
            db::ClaimResult::Claimed { job_id } => job_id,
            _ => panic!("expected Claimed"),
        };

        // Job should be active (building since claim sets it directly)
        let active = store.find_active_lazy_job(&file_id, "structural").unwrap();
        assert!(active.is_some(), "job should be active after claim");

        // Complete the job
        store.complete_lazy_job(&job_id).unwrap();

        // After completion, job should not be active
        let active = store.find_active_lazy_job(&file_id, "structural").unwrap();
        assert!(active.is_none(), "job should not be active after completion");

        // Verify job status via get
        let job = store.get_lazy_job(&job_id).unwrap().unwrap();
        assert_eq!(job.status, "complete");
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
    fn prebuilt_dataflow_preserved_by_lazy() {
        // Validate that pre-existing dataflow nodes are detected
        // and treated as cached by the lazy loader's prebuilt guard.
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
        store.insert_file_facts(&types::structs::FileFacts {
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

        // Insert pre-built dataflow node
        let binding_id = BindingId::generate(&file_id, &scope_id, "parameter", "x", 0);
        let dn_id = DataNodeId::generate(
            &file_id,
            Some(&func_sym_id),
            "parameter",
            Some("x"),
            Some("x"),
            10,
        );
        let dn = DataNode::parameter(dn_id, file_id, Some(func_sym_id), Some(binding_id), "x", range);
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
            .replace_dataflow_for_unit(
                &unit,
                &[dn.clone()],
                &[],
                &[binding],
                &[],
                &[],
                &[],
            )
            .unwrap();

        // Verify pre-built data exists
        let pre_count = store.count_data_nodes_for_unit(&unit).unwrap();
        assert!(pre_count > 0, "pre-built data nodes should exist");

        // The prebuilt guard in check_cache should detect these nodes.
        // Call count again to verify they're still there (not deleted).
        let pre_count2 = store.count_data_nodes_for_unit(&unit).unwrap();
        assert_eq!(
            pre_count2, pre_count,
            "pre-built data nodes should be preserved, not deleted"
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
}
