//! ClosureEngine — build focus closures via bounded fixed-point iteration.
//!
//! # Design
//!
//! The engine orchestrates the plan → extract → resolve pipeline for a
//! user-provided [`FocusWindow`].  Each iteration:
//!
//! 1. **Plan**: ask strategies what files to add next
//! 2. **Extract**: use [`LazyStructuralService`] to extract un-visited files
//! 3. **Resolve**: call [`ReferenceResolver::resolve_for_closure`] to resolve
//!    references within the closure scope
//! 4. Repeat until fixed point, budget exhausted, or max iterations reached
//!
//! # Strategies
//!
//! - [`ClosureStrategy::ImportNeighborhood`] delegates to [`ClosurePlanner`].
//! - [`ClosureStrategy::SameDirectory`] uses [`Store::list_file_ids_in_scope`].
//! - [`ClosureStrategy::CallGraph`] expands via scoped reference resolutions
//!   from the DB (calls/inbound calls), limited to depth=1 per iteration with
//!   multi-hop handled by fixed-point re-query.
//! - [`ClosureStrategy::TypeGraph`] expands via pre-computed type-reference
//!   traversal using type symbol kinds (Struct/Class/Enum/Interface/Trait).
//!
//! # Visibility
//!
//! Built closures are committed atomically via [`Store::commit_closure_generation`]
//! and [`Store::make_coverage_visible`], which gates MCP query results.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use db::Store;
use resolution::ReferenceResolver;
use types::enums::{Language, ReferenceKind, SymbolKind};
use types::ids::{FileId, SymbolId};
use types::structs::{KnownGap, SymbolDef};

use crate::LazyDataflowService;
use crate::closure_planner::{ClosurePlanner, IncludeRoot};
use crate::focus::focus_graph_builder::FocusGraphBuilder;
use crate::focus::visibility_filter::{VisibilityContext, VisibilityFilterRegistry};
use crate::lazy_budget::LazyBudget;
use crate::lazy_structural::{EnsureStructuralResult, LazyStructuralService};

use super::types::{ClosureStrategy, Direction, FocusClosure, FocusSeed, FocusWindow};

/// Engine for building focus closures around a user's seed.
///
/// Implements bounded fixed-point iteration: plan → extract → resolve → plan
/// until no additions, budget exhausted, or max_iterations reached.
pub struct ClosureEngine {
    pub(crate) store: Arc<Store>,
    pub(crate) lazy_structural: LazyStructuralService,
    pub(crate) dataflow: LazyDataflowService,
    pub(crate) resolver: RefCell<ReferenceResolver>,
    pub(crate) graph_builder: FocusGraphBuilder,
    pub(crate) project_root: Option<std::path::PathBuf>,
    pub(crate) include_roots: Vec<IncludeRoot>,
}

impl ClosureEngine {
    pub fn new(
        store: Arc<Store>,
        lazy_structural: LazyStructuralService,
        lazy_dataflow: LazyDataflowService,
        project_root: Option<std::path::PathBuf>,
        include_roots: Vec<IncludeRoot>,
    ) -> Self {
        let resolver = RefCell::new(ReferenceResolver::new(store.clone()));
        let graph_builder = FocusGraphBuilder::new(store.clone());
        ClosureEngine {
            store,
            lazy_structural,
            dataflow: lazy_dataflow,
            resolver,
            graph_builder,
            project_root,
            include_roots,
        }
    }

    /// Build a focus closure with bounded fixed-point iteration.
    ///
    /// 1. Initialize closure from seed
    /// 2. Plan additions via strategies
    /// 3. Extract new files using LazyStructuralService
    /// 4. Resolve references scoped to closure (writes to reference_resolutions)
    /// 5. Repeat until termination
    /// 6. Commit visibility atomically (coverage + resolutions)
    pub fn build_closure(&self, window: &FocusWindow, closure_id: &str) -> Result<FocusClosure> {
        let started_at = Instant::now();
        let deadline = started_at + Duration::from_millis(window.budget.max_time_ms);
        // Insert closure generation record
        self.store.insert_closure_generation(closure_id)?;

        let mut closure = FocusClosure::new(&window.seed);
        let mut iteration: u32 = 0;

        // Phase 1: locate and extract seed file(s)
        let seed_files = self.locate_seed(&window.seed)?;
        let generation: i64 = 0; // generation 0 = seed extraction
        for file_id in &seed_files {
            // Design note: the first seed file is intentionally exempted from
            // the time budget (the check only fires when `!closure.files.is_empty()`).
            // This ensures at least one file is extracted even under tight deadlines —
            // returning an empty closure would be useless to the caller.
            if !closure.files.is_empty() && Instant::now() >= deadline {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: "seed_time_budget".to_string(),
                    remaining: seed_files.len().saturating_sub(closure.files.len()),
                });
                break;
            }
            let result = self.extract_file(file_id)?;
            closure.mark_extracted(*file_id, &result.precision);

            // Populate closure symbols from the extracted file so graph-based
            // strategies (CallGraph, TypeGraph) can query edges by source symbol.
            if let Ok(symbols) = self.store.find_symbols_by_file(file_id) {
                for sym in &symbols {
                    closure.symbols.insert(sym.id);
                }
            }

            // Record coverage for the seed file
            self.store.insert_closure_coverage(
                closure_id,
                file_id.as_bytes(),
                "seed_file",
                generation,
                None,
            )?;
        }

        // Incremental resolution tracking: avoid re-resolving files that have
        // already been resolved. Seed files are resolved immediately so that
        // CallGraph can find their call targets on the first loop iteration.
        let mut previously_resolved: HashSet<FileId> = HashSet::new();
        if !seed_files.is_empty() {
            // Build visibility filter for seed file resolution.
            // NOTE: from_file uses FileId::default() because closure-scoped
            // resolution doesn't have a single "calling" file — it resolves
            // across all seed files. Per-reference visibility still works
            // correctly because the filter's `from_file` parameter is set
            // by the resolver for each individual reference.
            let visibility_filter = self.build_visibility_filter(&closure.files);
            let filter_ref: Option<&dyn Fn(&SymbolDef, FileId) -> bool> =
                visibility_filter.as_deref();
            self.resolver.borrow_mut().resolve_for_closure(
                closure_id,
                0, // generation 0 = seed resolution
                &seed_files,
                filter_ref,
            )?;
            for f in &seed_files {
                previously_resolved.insert(*f);
            }
        }

        // Phase 2: pre-compute TypeGraph expansion (single-pass, not iterative).
        // TypeGraph handles its own depth internally — we do NOT re-invoke it
        // inside the fixed-point loop below.
        let mut pre_additions = Vec::new();
        if Instant::now() < deadline {
            for strategy in &window.strategies {
                if let ClosureStrategy::TypeGraph { max_depth } = strategy {
                    let type_files = self.expand_types(&closure, *max_depth, closure_id)?;
                    for f in type_files {
                        if !closure.visited.contains(&f) {
                            pre_additions.push(f);
                        }
                    }
                }
            }
        } else {
            closure.record_gap(KnownGap::BudgetExhausted {
                strategy: "typegraph_time_budget".to_string(),
                remaining: 0,
            });
        }

        // Phase 3: bounded fixed-point expansion
        let mut last_generation: i64 = 0; // generation 0 = seed extraction
        loop {
            if Instant::now() >= deadline {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: "closure_time_budget".to_string(),
                    remaining: 0,
                });
                break;
            }
            iteration += 1;

            // Plan: ask strategies what to add next.
            // Merge pre-computed TypeGraph results into the first iteration.
            let additions = if iteration == 1 && !pre_additions.is_empty() {
                let mut plan = self.plan_additions(&window.strategies, &closure, closure_id)?;
                for f in &pre_additions {
                    if !plan.contains(f) {
                        plan.push(*f);
                    }
                }
                plan
            } else {
                self.plan_additions(&window.strategies, &closure, closure_id)?
            };

            if additions.is_empty() {
                break; // fixed point reached
            }

            // Budget check
            if !window.budget.can_absorb(&additions) || additions.len() > window.budget.max_files {
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
            last_generation = generation;
            for file_id in &new_files {
                if Instant::now() >= deadline {
                    closure.record_gap(KnownGap::BudgetExhausted {
                        strategy: format!("iteration {iteration} time_budget"),
                        remaining: new_files.len(),
                    });
                    break;
                }
                let result = self.extract_file(file_id)?;
                closure.mark_extracted(*file_id, &result.precision);

                // Populate closure symbols from the extracted file
                if let Ok(symbols) = self.store.find_symbols_by_file(file_id) {
                    for sym in &symbols {
                        closure.symbols.insert(sym.id);
                    }
                }

                // Record coverage
                self.store.insert_closure_coverage(
                    closure_id,
                    file_id.as_bytes(),
                    "extracted_structural",
                    generation,
                    None,
                )?;
            }

            // Incremental resolution: resolve only newly extracted files so
            // that the next iteration's CallGraph expansion can discover call
            // targets via reference_resolutions.
            let truly_new: Vec<FileId> = new_files
                .iter()
                .filter(|f| !previously_resolved.contains(f))
                .copied()
                .collect();
            if !truly_new.is_empty() {
                self.resolver.borrow_mut().resolve_for_closure(
                    closure_id, generation, &truly_new,
                    None, // no visibility filter during incremental resolution
                )?;
                for f in &truly_new {
                    previously_resolved.insert(*f);
                }
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

        // Resolve references scoped to this closure (writes to reference_resolutions).
        // Language-specific visibility filter excludes symbols that are not
        // reachable from the reference's file (e.g., C static, Rust private).
        let closure_files: Vec<FileId> = closure.files.iter().copied().collect();
        if !closure_files.is_empty() && Instant::now() < deadline {
            let visibility_filter = self.build_visibility_filter(&closure.files);
            let filter_ref: Option<&dyn Fn(&SymbolDef, FileId) -> bool> =
                visibility_filter.as_deref();
            self.resolver.borrow_mut().resolve_for_closure(
                closure_id,
                last_generation,
                &closure_files,
                filter_ref,
            )?;
        } else if !closure_files.is_empty() {
            closure.record_gap(KnownGap::BudgetExhausted {
                strategy: "final_resolution_time_budget".to_string(),
                remaining: closure_files.len(),
            });
        }

        // Commit: atomic visibility switch
        self.commit_closure(closure_id)?;

        // Build scoped graph edges from closure resolutions.
        // FocusGraphBuilder reads from reference_resolutions (is_visible=1)
        // and routes edges via EdgeConflictPolicy.
        if Instant::now() < deadline {
            let stats = self
                .graph_builder
                .build_for_closure(closure_id, last_generation)?;
            if stats.candidate_count > 0 {
                self.store
                    .make_candidate_edges_visible(closure_id, last_generation)?;
            }
            tracing::debug!(
                closure_id = %closure_id,
                edges_built = stats.stats.edges_built,
                edges_written = stats.stats.edges_written,
                candidate_count = stats.candidate_count,
                "FocusGraphBuilder completed"
            );
        } else {
            closure.record_gap(KnownGap::BudgetExhausted {
                strategy: "graph_build_time_budget".to_string(),
                remaining: closure.files.len(),
            });
        }

        Ok(closure)
    }

    /// Trigger dataflow extraction for all functions in closure files.
    ///
    /// Called after [`build_closure`] completes graph building at the
    /// structural level.  Walks every file in the closure, finds
    /// Function/Method symbols, and invokes
    /// [`LazyDataflowService::ensure_for_function`] for each.  Errors are
    /// logged at debug level and do not propagate — dataflow building is
    /// opportunistic background work.
    ///
    /// Returns the number of functions for which dataflow was successfully
    /// planned and ensured.
    pub fn build_dataflow_for_closure(
        &self,
        closure_id: &str,
        files: &HashSet<FileId>,
    ) -> Result<usize> {
        let mut built = 0;
        for file_id in files {
            let symbols = self.store.find_symbols_by_file(file_id)?;
            for sym in &symbols {
                if matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                    match self.dataflow.ensure_for_function(&sym.id, Some(closure_id)) {
                        Ok(_) => built += 1,
                        Err(e) => tracing::debug!(%e, symbol=%sym.name, "dataflow build failed"),
                    }
                }
            }
        }
        Ok(built)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Build a visibility filter for resolving references scoped to a closure.
    ///
    /// Constructs a language-specific `Fn(&SymbolDef, FileId) -> bool` that
    /// checks whether a symbol is visible from a given file. Uses
    /// `FileId::default()` as the "from" context because closure-scoped
    /// resolution operates across all files in the closure (visibility
    /// decisions are per-reference, not per-closure-file).
    ///
    /// The `VisibilityFilterRegistry` is owned by the returned closure so
    /// the filter's lifetime is self-contained — callers do not need to
    /// hold a reference to the registry.
    ///
    /// Returns `None` when the closure contains no files with a known language.
    fn build_visibility_filter(
        &self,
        closure_files: &HashSet<FileId>,
    ) -> Option<Box<dyn Fn(&SymbolDef, FileId) -> bool>> {
        let language: Option<Language> = closure_files
            .iter()
            .filter_map(|file_id| self.store.get_file(file_id).ok().flatten())
            .map(|file_info| file_info.language)
            .next();

        // The registry is moved into the closure, so the filter's lifetime
        // is self-contained. This is the same pattern as the original inline
        // code — the registry exists only as long as the closure.
        let registry = VisibilityFilterRegistry::new();
        language.map(move |lang| {
            // We need the filter from the registry. Since registry is moved
            // into this closure, we extract the filter result by calling
            // get() and then capturing it in an inner closure.
            //
            // However, get() returns &dyn VisibilityFilter which borrows
            // from registry. We work around this by storing the registry
            // inside the closure and calling get() on each invocation.
            let ctx = VisibilityContext {
                from_file: FileId::default(),
                from_crate_root: None,
                target_crate_root: None,
            };
            Box::new(move |sym: &SymbolDef, from_file: FileId| -> bool {
                registry.get(lang).is_visible(sym, from_file, &ctx)
            }) as Box<dyn Fn(&SymbolDef, FileId) -> bool>
        })
    }

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

    /// Expand closure through call-graph references.
    ///
    /// Queries `reference_resolutions` (populated by incremental scoped
    /// resolution) instead of `symbol_edges` / `symbol_edge_candidates`
    /// (which are built after the fixed-point loop completes).
    ///
    /// For `Direction::Outgoing`: finds Call references made by closure
    /// symbols and resolves their targets to containing files.
    ///
    /// For `Direction::Incoming`: finds Call references whose resolved
    /// target is a closure symbol and resolves the source symbol to its file.
    ///
    /// For `Direction::Both`: combines Outgoing + Incoming results.
    ///
    /// `depth != 1` returns empty because multi-hop expansion is deferred to
    /// fixed-point iteration.
    fn expand_callgraph(
        &self,
        closure: &FocusClosure,
        closure_id: &str,
        direction: Direction,
        depth: u32,
    ) -> Result<Vec<FileId>> {
        // Only single-level expansion is supported; multi-hop is handled by
        // the fixed-point loop (each iteration re-queries with newly extracted
        // symbols).
        if depth != 1 {
            tracing::warn!(
                "Non-default depth requested: {}, focus analysis only supports depth=1",
                depth
            );
            return Ok(Vec::new());
        }

        let mut result: HashSet<FileId> = HashSet::new();
        let ref_kind = ReferenceKind::Call.as_str();

        let do_outgoing = matches!(direction, Direction::Outgoing | Direction::Both);
        let do_incoming = matches!(direction, Direction::Incoming | Direction::Both);

        if do_outgoing {
            for sym_id in &closure.symbols {
                let targets = self.store.get_resolved_targets_for_symbol_in_closure(
                    closure_id,
                    sym_id.as_bytes(),
                    ref_kind,
                )?;
                for target_blob in &targets {
                    if target_blob.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(target_blob);
                        let target_sym_id = SymbolId::from_bytes(arr);
                        if let Ok(Some(file_id)) = self.store.find_symbol_file(&target_sym_id) {
                            if !closure.visited.contains(&file_id) {
                                result.insert(file_id);
                            }
                        }
                    }
                }
            }
        }

        if do_incoming {
            for sym_id in &closure.symbols {
                let callers = self.store.get_callers_for_symbol_in_closure(
                    closure_id,
                    sym_id.as_bytes(),
                    ref_kind,
                )?;
                for source_blob in &callers {
                    if source_blob.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(source_blob);
                        let source_sym_id = SymbolId::from_bytes(arr);
                        if let Ok(Some(file_id)) = self.store.find_symbol_file(&source_sym_id) {
                            if !closure.visited.contains(&file_id) {
                                result.insert(file_id);
                            }
                        }
                    }
                }
            }
        }

        Ok(result.into_iter().collect())
    }

    /// Plan additions based on strategies + current closure state.
    fn plan_additions(
        &self,
        strategies: &[ClosureStrategy],
        closure: &FocusClosure,
        closure_id: &str,
    ) -> Result<Vec<FileId>> {
        let mut additions = Vec::new();

        for strategy in strategies {
            match strategy {
                ClosureStrategy::ImportNeighborhood { depth } => {
                    // Create a ClosurePlanner for import expansion; plan_closure
                    // takes &self so we reuse the same instance for all files.
                    let planner =
                        ClosurePlanner::new(self.store.clone(), self.project_root.clone())
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
                ClosureStrategy::CallGraph { direction, depth } => {
                    let callee_files =
                        self.expand_callgraph(closure, closure_id, *direction, *depth)?;
                    additions.extend(callee_files);
                }
                ClosureStrategy::TypeGraph { .. } => {
                    // TypeGraph is pre-computed before the fixed-point loop.
                    // See Phase 2 in build_closure().
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

    /// Commit closure: make all staged coverage and resolution entries visible.
    fn commit_closure(&self, closure_id: &str) -> Result<i64> {
        // P1a fix: make ALL staged rows visible regardless of generation.
        // Seed files are written with generation=0, expansion files with
        // generation=iteration. We must flip all of them, not just the
        // committed generation number.
        let generation = self.store.commit_closure_generation(closure_id)?;
        self.store.make_all_staged_coverage_visible(closure_id)?;
        // T6/D: make scoped reference resolutions visible too
        self.store.make_all_staged_resolutions_visible(closure_id)?;
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

    /// Expand closure through type dependencies.
    ///
    /// For each file in scope, find references whose resolved target (from
    /// closure-scoped [`reference_resolutions`]) is a type definition
    /// (Struct/Class/Enum/Interface/Trait).  Add the file containing that
    /// type definition to the closure.  When `max_depth > 1`, repeat the
    /// process for the newly added files to discover transitive type
    /// dependencies.
    ///
    /// Uses closure-scoped resolution data (not the global `references`
    /// table) so that resolutions produced by incremental closure resolution
    /// are visible to TypeGraph expansion.
    fn expand_types(
        &self,
        closure: &FocusClosure,
        max_depth: u32,
        closure_id: &str,
    ) -> Result<Vec<FileId>> {
        let type_ref_kinds = [
            ReferenceKind::Usage,
            ReferenceKind::Inheritance,
            ReferenceKind::Implementation,
        ];
        let type_symbol_kinds = [
            SymbolKind::Struct,
            SymbolKind::Class,
            SymbolKind::Enum,
            SymbolKind::Interface,
            SymbolKind::Trait,
        ];

        let mut all_additions: Vec<FileId> = Vec::new();
        // At each depth level we only search the files added in the previous
        // level so that each depth step corresponds to one hop in the type
        // dependency chain.
        let mut current_scope: HashSet<FileId> = closure.files.iter().copied().collect();

        for _depth in 0..max_depth {
            let mut depth_additions = Vec::new();

            for file_id in &current_scope {
                // Collect all resolved target symbol IDs from both closure-scoped
                // and global sources.  Closure-scoped resolutions (from
                // reference_resolutions) take priority; the global references
                // table acts as a fallback for files that have not yet been
                // resolved by the closure-scoped resolver (e.g., multi-depth
                // TypeGraph where depth-1 files are not resolved until the
                // fixed-point loop).
                let mut resolved_target_ids: HashSet<SymbolId> = HashSet::new();

                // 1. Closure-scoped resolutions (primary source)
                let closure_targets = self
                    .store
                    .get_resolved_targets_for_file_and_kinds_in_closure(
                        closure_id,
                        file_id,
                        &type_ref_kinds,
                    )?;
                for (target_blob, _ref_kind) in &closure_targets {
                    if target_blob.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(target_blob);
                        resolved_target_ids.insert(SymbolId::from_bytes(arr));
                    } else {
                        tracing::warn!(
                            "expand_types: closure target_blob has unexpected length {} (expected 32), skipping",
                            target_blob.len()
                        );
                    }
                }

                // 2. Global references table (fallback)
                let global_refs = self
                    .store
                    .find_references_by_file_and_kinds(file_id, &type_ref_kinds)?;
                for r in &global_refs {
                    if let Some(resolved) = &r.resolved {
                        resolved_target_ids.insert(resolved.symbol_id);
                    }
                }

                for target_id in &resolved_target_ids {
                    // Check if the resolved target is a type definition
                    let kind = match self.store.get_symbol_kind(target_id)? {
                        Some(k) => k,
                        None => continue,
                    };

                    if !type_symbol_kinds.contains(&kind) {
                        continue;
                    }

                    // Get the file containing this type symbol
                    let sym = match self.store.find_symbol_by_id(target_id)? {
                        Some(s) => s,
                        None => continue,
                    };

                    let target_file = sym.file_id;
                    if !closure.visited.contains(&target_file)
                        && !all_additions.contains(&target_file)
                        && !depth_additions.contains(&target_file)
                    {
                        depth_additions.push(target_file);
                    }
                }
            }

            if depth_additions.is_empty() {
                break; // no more transitive type deps to discover
            }

            all_additions.extend(depth_additions.iter().copied());
            current_scope = depth_additions.into_iter().collect();
        }

        Ok(all_additions)
    }
}
