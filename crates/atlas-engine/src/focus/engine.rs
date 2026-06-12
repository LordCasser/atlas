//! ClosureEngine — build focus closures via bounded fixed-point iteration.
//!
//! # Design
//!
//! The engine orchestrates the plan → extract → resolve pipeline for a
//! user-provided [`FocusWindow`].  Each iteration:
//!
//! 1. **Plan**: ask strategies what files to add next
//! 2. **Extract**: use [`LazyStructuralService`] to extract un-visited files
//! 3. **Resolve**: (stub — full resolution in T6)
//! 4. Repeat until fixed point, budget exhausted, or max iterations reached
//!
//! # Strategies
//!
//! - [`ClosureStrategy::ImportNeighborhood`] delegates to [`ClosurePlanner`].
//! - [`ClosureStrategy::SameDirectory`] uses [`Store::list_file_ids_in_scope`].
//! - [`ClosureStrategy::CallGraph`] and [`ClosureStrategy::TypeGraph`] are
//!   stubs for this phase.
//!
//! # Visibility
//!
//! Built closures are committed atomically via [`Store::commit_closure_generation`]
//! and [`Store::make_coverage_visible`], which gates MCP query results.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use db::Store;
use types::ids::FileId;
use types::structs::KnownGap;

use crate::closure_planner::{ClosurePlanner, IncludeRoot};
use crate::lazy_budget::LazyBudget;
use crate::lazy_structural::{EnsureStructuralResult, LazyStructuralService};

use super::types::{
    ClosureStrategy, FocusClosure, FocusSeed, FocusWindow,
};

/// Engine for building focus closures around a user's seed.
///
/// Implements bounded fixed-point iteration: plan → extract → resolve → plan
/// until no additions, budget exhausted, or max_iterations reached.
pub struct ClosureEngine {
    pub(crate) store: Arc<Store>,
    pub(crate) lazy_structural: LazyStructuralService,
    pub(crate) project_root: Option<std::path::PathBuf>,
    pub(crate) include_roots: Vec<IncludeRoot>,
}

impl ClosureEngine {
    pub fn new(
        store: Arc<Store>,
        lazy_structural: LazyStructuralService,
        project_root: Option<std::path::PathBuf>,
        include_roots: Vec<IncludeRoot>,
    ) -> Self {
        ClosureEngine {
            store,
            lazy_structural,
            project_root,
            include_roots,
        }
    }

    /// Build a focus closure with bounded fixed-point iteration.
    ///
    /// 1. Initialize closure from seed
    /// 2. Plan additions via strategies
    /// 3. Extract new files using LazyStructuralService
    /// 4. Resolve scoped to closure (stub — full resolution in T6)
    /// 5. Repeat until termination
    /// 6. Commit visibility atomically
    pub fn build_closure(
        &self,
        window: &FocusWindow,
        closure_id: &str,
    ) -> Result<FocusClosure> {
        // Insert closure generation record
        self.store.insert_closure_generation(closure_id)?;

        let mut closure = FocusClosure::new(&window.seed);
        let mut iteration: u32 = 0;

        // Phase 1: locate and extract seed file(s)
        let seed_files = self.locate_seed(&window.seed)?;
        let generation: i64 = 0; // generation 0 = seed extraction
        for file_id in &seed_files {
            let result = self.extract_file(file_id)?;
            closure.mark_extracted(*file_id, result.precision_tier);

            // Record coverage for the seed file
            self.store.insert_closure_coverage(
                closure_id,
                file_id.as_bytes(),
                "seed_file",
                generation,
                None,
                &format!("{:?}", result.precision_tier),
            )?;
        }

        // Phase 2: bounded fixed-point expansion
        loop {
            iteration += 1;

            // Plan: ask strategies what to add next
            let additions = self.plan_additions(&window.strategies, &closure)?;

            if additions.is_empty() {
                break; // fixed point reached
            }

            // Budget check
            if !window.budget.can_absorb(&additions)
                || additions.len() > window.budget.max_files
            {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: format!("iteration {iteration}"),
                    remaining: additions.len(),
                });
                break;
            }

            // Filter: only files not already visited
            let new_files: Vec<FileId> = additions
                .into_iter()
                .filter(|f| !closure.visited.contains(f))
                .collect();

            if new_files.is_empty() {
                break;
            }

            // Extract new files using LazyStructuralService
            let generation = iteration as i64;
            for file_id in &new_files {
                let result = self.extract_file(file_id)?;
                closure.mark_extracted(*file_id, result.precision_tier);

                // Record coverage
                self.store.insert_closure_coverage(
                    closure_id,
                    file_id.as_bytes(),
                    "extracted_structural",
                    generation,
                    None,
                    &format!("{:?}", result.precision_tier),
                )?;
            }

            // Termination check
            if iteration >= window.max_iterations {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: "max_iterations".to_string(),
                    remaining: 0,
                });
                break;
            }
        }

        // Commit: atomic visibility switch
        self.commit_closure(closure_id)?;

        Ok(closure)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Locate files containing the seed.
    fn locate_seed(&self, seed: &FocusSeed) -> Result<Vec<FileId>> {
        match seed {
            FocusSeed::File { file_id, .. } => Ok(vec![*file_id]),
            FocusSeed::Position { file_id, .. } => Ok(vec![*file_id]),
            FocusSeed::Symbol { name, .. } => {
                // Use candidate provider from lazy_structural
                self.lazy_structural
                    .candidate_provider
                    .candidates_for_symbol(name)
                    .map(|ids| ids.into_iter().take(5).collect())
                    .context("Failed to locate seed symbol")
            }
            FocusSeed::Field { struct_sym, .. } => {
                // Look up struct symbol's file
                let sym = self
                    .store
                    .find_symbol_by_id(struct_sym)?
                    .context("Seed struct symbol not found")?;
                Ok(vec![sym.file_id])
            }
        }
    }

    /// Plan additions based on strategies + current closure state.
    fn plan_additions(
        &self,
        strategies: &[ClosureStrategy],
        closure: &FocusClosure,
    ) -> Result<Vec<FileId>> {
        let mut additions = Vec::new();

        for strategy in strategies {
            match strategy {
                ClosureStrategy::ImportNeighborhood { depth } => {
                    // Create a ClosurePlanner for import expansion; plan_closure
                    // takes &self so we reuse the same instance for all files.
                    let planner = ClosurePlanner::new(
                        self.store.clone(),
                        self.project_root.clone(),
                    )
                    .with_include_roots(self.include_roots.clone())
                    .with_limits(*depth as usize, 30);

                    for file_id in &closure.files {
                        let deps = planner.plan_closure(file_id)?;
                        for dep in &deps.direct_deps {
                            if !closure.visited.contains(dep) {
                                additions.push(*dep);
                            }
                        }
                        for dep in &deps.transitive_deps {
                            if !closure.visited.contains(dep) {
                                additions.push(*dep);
                            }
                        }
                    }
                }
                ClosureStrategy::SameDirectory => {
                    // Find sibling files in same directories as closure files
                    let dirs: HashSet<String> = closure
                        .files
                        .iter()
                        .filter_map(|fid| self.get_file_directory(fid))
                        .collect();

                    for dir in &dirs {
                        let siblings = self.find_directory_files(dir)?;
                        for sib in siblings {
                            if !closure.visited.contains(&sib) {
                                additions.push(sib);
                            }
                        }
                    }
                }
                ClosureStrategy::CallGraph { .. } => {
                    // Stub: expand through call graph edges.
                    // Full implementation will use GraphEngine for callee/caller
                    // expansion when graph-based strategy support is added.
                }
                ClosureStrategy::TypeGraph { .. } => {
                    // Stub: expand through type references.
                    // Full implementation will use type-definition resolution
                    // when type-graph traversal is added.
                }
            }
        }

        // Dedup
        additions.sort();
        additions.dedup();
        additions.retain(|f| !closure.visited.contains(f));

        Ok(additions)
    }

    /// Extract a single file using LazyStructuralService.
    fn extract_file(&self, file_id: &FileId) -> Result<EnsureStructuralResult> {
        // Create a budget for this extraction — LazyBudget implements CancelCheck
        // so extraction can be cancelled at checkpoints when time/quota exhausted.
        let budget = LazyBudget::structural();
        let result = self
            .lazy_structural
            .ensure_structural_for_file(file_id, Some(&budget))?;
        Ok(result)
    }

    /// Commit closure: make all staged coverage entries visible.
    fn commit_closure(&self, closure_id: &str) -> Result<i64> {
        // P1a fix: make ALL staged rows visible regardless of generation.
        // Seed files are written with generation=0, expansion files with
        // generation=iteration. We must flip all of them, not just the
        // committed generation number.
        let generation = self.store.commit_closure_generation(closure_id)?;
        self.store.make_all_staged_coverage_visible(closure_id)?;
        Ok(generation)
    }

    /// Get the directory of a file (for SameDirectory strategy).
    fn get_file_directory(&self, file_id: &FileId) -> Option<String> {
        let file_info = self.store.get_file(file_id).ok()??;
        let parent = std::path::Path::new(&file_info.path).parent()?;
        Some(parent.to_string_lossy().to_string())
    }

    /// Find all file IDs in a directory.
    fn find_directory_files(&self, dir: &str) -> Result<Vec<FileId>> {
        // Use list_file_ids_in_scope with a generous limit.
        // An empty string is the project root, which the scope function
        // normalizes — for a specific directory we pass the directory path.
        self.store.list_file_ids_in_scope(dir, 100)
    }
}
