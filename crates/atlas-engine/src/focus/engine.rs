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
//! - [`ClosureStrategy::ImportNeighborhood`] delegates to `ClosurePlanner`.
//! - [`ClosureStrategy::SameDirectory`] uses [`Store::list_file_ids_in_scope`].
//! - [`ClosureStrategy::CallGraph`] expands via scoped reference resolutions
//!   from the DB (calls/inbound calls), limited to depth=1 per iteration with
//!   multi-hop handled by fixed-point re-query.
//! - [`ClosureStrategy::TypeGraph`] expands via pre-computed type-reference
//!   traversal using type symbol kinds (Struct/Class/Enum/Interface/Trait).
//! - [`ClosureStrategy::StateChannel`] discovers files that write framework
//!   state referenced by the seed closure.
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
use resolution::{ReferenceResolver, VisibilityFilterFn};
use types::enums::{Language, ReferenceKind, SymbolKind};
use types::ids::{FileId, SymbolId};
use types::structs::{KnownGap, SymbolDef};

use crate::closure_planner::ClosurePlanner;
use crate::focus::focus_graph_builder::FocusGraphBuilder;
use crate::focus::materialize::{EnsureStructuralResult, FocusMaterialize};
use crate::focus::visibility_filter::{VisibilityContext, VisibilityFilterRegistry};
use crate::lazy_budget::LazyBudget;

use super::types::{ClosureStrategy, Direction, FocusClosure, FocusSeed, FocusWindow};

const RETAINED_COMMITTED_CLOSURES: usize = 16;

fn required_resolution_kinds(strategies: &[ClosureStrategy]) -> HashSet<ReferenceKind> {
    let mut kinds = HashSet::new();
    for strategy in strategies {
        match strategy {
            ClosureStrategy::CallGraph { .. } => {
                kinds.insert(ReferenceKind::Call);
            }
            ClosureStrategy::TypeGraph { .. } => {
                kinds.extend([
                    ReferenceKind::Usage,
                    ReferenceKind::TypeReference,
                    ReferenceKind::Inheritance,
                    ReferenceKind::Implementation,
                ]);
            }
            ClosureStrategy::ImportNeighborhood { .. }
            | ClosureStrategy::SameDirectory
            | ClosureStrategy::StateChannel => {}
        }
    }
    kinds
}

/// Engine for building focus closures around a user's seed.
///
/// Implements bounded fixed-point iteration: plan → extract → resolve → plan
/// until no additions, budget exhausted, or max_iterations reached.
pub struct ClosureEngine {
    pub(crate) store: Arc<Store>,
    /// Shared Focus materialize stack (structural + dataflow).
    pub(crate) materialize: FocusMaterialize,
    pub(crate) resolver: RefCell<ReferenceResolver>,
    pub(crate) graph_builder: FocusGraphBuilder,
}

impl ClosureEngine {
    pub fn new(store: Arc<Store>, materialize: FocusMaterialize) -> Self {
        let resolver = RefCell::new(ReferenceResolver::new(store.clone()));
        let graph_builder = FocusGraphBuilder::new(store.clone());
        ClosureEngine {
            store,
            materialize,
            resolver,
            graph_builder,
        }
    }

    /// Build a focus closure with bounded fixed-point iteration.
    ///
    /// 1. Initialize closure from seed
    /// 2. Plan additions via strategies
    /// 3. Extract new files using Focus structural materialize
    /// 4. Resolve references scoped to closure (writes to reference_resolutions)
    /// 5. Repeat until termination
    /// 6. Commit visibility atomically (coverage + resolutions)
    pub fn build_closure(&self, window: &FocusWindow, closure_id: &str) -> Result<FocusClosure> {
        let started_at = Instant::now();
        let deadline = started_at + Duration::from_millis(window.budget.max_time_ms);
        let extraction_budget = LazyBudget::for_duration_ms(window.budget.max_time_ms);
        // Insert closure generation record
        self.store.insert_closure_generation(closure_id)?;

        let mut closure = FocusClosure::new(&window.seed);
        let mut iteration: u32 = 0;
        let mut expanded_files: usize = 0;
        let resolution_kinds = required_resolution_kinds(&window.strategies);

        // Phase 1: locate and extract seed file(s)
        let seed_files = self.locate_seed(&window.seed)?;
        let generation: i64 = 0; // generation 0 = seed extraction
        for file_id in &seed_files {
            // Cached seed files may still enter the closure under a tight time
            // window, but cold structural extraction respects the shared window
            // budget. This keeps large-file seeds from bypassing focus bounds.
            if !closure.files.is_empty() && Instant::now() >= deadline {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: "seed_time_budget".to_string(),
                    remaining: seed_files.len().saturating_sub(closure.files.len()),
                });
                break;
            }
            let result = self.extract_file_for_closure(file_id, &extraction_budget)?;
            tracing::debug!(
                closure_id,
                file_id = %file_id,
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "focus seed structural facts ready"
            );
            if !self.record_extraction_outcome(&mut closure, *file_id, &result) {
                continue;
            }

            // Populate closure symbols from the extracted file so graph-based
            // strategies (CallGraph, TypeGraph) can query edges by source symbol.
            if let Ok(symbols) = self.store.find_symbols_by_file(file_id) {
                for sym in &symbols {
                    if symbol_matches_seed(&closure.seed, sym) {
                        closure.symbols.insert(sym.id);
                    }
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

        for gap in self.materialize_import_dependencies(
            window,
            &closure,
            &window.strategies,
            closure_id,
            0,
            window.budget.max_files,
        )? {
            closure.record_gap(gap);
        }

        // Seed files are resolved after their dependency symbols are available
        // so CallGraph can find targets on the first loop iteration.
        if !seed_files.is_empty() {
            // Build visibility filter for seed file resolution.
            // NOTE: from_file uses FileId::default() because closure-scoped
            // resolution doesn't have a single "calling" file — it resolves
            // across all seed files. Per-reference visibility still works
            // correctly because the filter's `from_file` parameter is set
            // by the resolver for each individual reference.
            let visibility_filter = self.build_visibility_filter(&closure.files);
            let filter_ref: Option<&VisibilityFilterFn> = visibility_filter.as_deref();
            self.resolver.borrow_mut().resolve_for_closure_kinds(
                closure_id,
                0, // generation 0 = seed resolution
                &seed_files,
                filter_ref,
                Some(&resolution_kinds),
            )?;
            tracing::debug!(
                closure_id,
                reference_kinds = resolution_kinds.len(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "focus seed scoped resolution complete"
            );
        }

        // Phase 2: bounded fixed-point expansion
        let mut last_generation: i64 = 0; // generation 0 = seed extraction
        loop {
            if iteration >= window.max_iterations {
                break;
            }
            if Instant::now() >= deadline {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: "closure_time_budget".to_string(),
                    remaining: 0,
                });
                break;
            }
            iteration += 1;

            // Newly extracted facts participate in every subsequent planning
            // round, so call and type boundaries can advance to a fixed point.
            let (additions, relevant_symbols) =
                self.plan_additions(&window.strategies, &closure, closure_id, iteration)?;
            closure.symbols.extend(relevant_symbols);
            tracing::debug!(
                closure_id,
                iteration,
                additions = additions.len(),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "focus expansion plan ready"
            );

            if additions.is_empty() {
                break; // fixed point reached
            }

            // Consume as much of the deterministic plan as the remaining file
            // budget permits. Rejecting an oversized plan wholesale leaves a
            // useful closure stuck at its seed even though capacity remains.
            let remaining_capacity = window.budget.max_files.saturating_sub(expanded_files);
            if remaining_capacity == 0 {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: format!("iteration {iteration}"),
                    remaining: additions.len(),
                });
                break;
            }
            let mut additions = additions;
            let budget_truncated = additions.len() > remaining_capacity;
            if budget_truncated {
                closure.record_gap(KnownGap::BudgetExhausted {
                    strategy: format!("iteration {iteration}"),
                    remaining: additions.len() - remaining_capacity,
                });
                additions.truncate(remaining_capacity);
            }

            // Filter: only files not already visited
            let new_files: Vec<FileId> = additions
                .into_iter()
                .filter(|f| !closure.visited.contains(f))
                .collect();

            if new_files.is_empty() {
                break;
            }
            expanded_files += new_files.len();

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
                let file_started_at = Instant::now();
                let result = self.extract_file_for_closure(file_id, &extraction_budget)?;
                tracing::debug!(
                    closure_id,
                    iteration,
                    file_id = %file_id,
                    file_elapsed_ms = file_started_at.elapsed().as_millis() as u64,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "focus expansion structural facts ready"
                );
                if !self.record_extraction_outcome(&mut closure, *file_id, &result) {
                    continue;
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

            for gap in self.materialize_import_dependencies(
                window,
                &closure,
                &window.strategies,
                closure_id,
                generation,
                window.budget.max_files,
            )? {
                closure.record_gap(gap);
            }

            // New dependency symbols can change resolutions in files that were
            // already visited, so refresh the complete bounded closure.
            let closure_files: Vec<FileId> = closure.files.iter().copied().collect();
            if !closure_files.is_empty() {
                self.resolver.borrow_mut().resolve_for_closure_kinds(
                    closure_id,
                    generation,
                    &closure_files,
                    None, // no visibility filter during incremental resolution
                    Some(&resolution_kinds),
                )?;
            }

            if budget_truncated {
                break;
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
            let filter_ref: Option<&VisibilityFilterFn> = visibility_filter.as_deref();
            self.resolver.borrow_mut().resolve_for_closure_kinds(
                closure_id,
                last_generation,
                &closure_files,
                filter_ref,
                Some(&resolution_kinds),
            )?;
        } else if !closure_files.is_empty() {
            closure.record_gap(KnownGap::BudgetExhausted {
                strategy: "final_resolution_time_budget".to_string(),
                remaining: closure_files.len(),
            });
        }

        // Commit: atomic visibility switch
        self.commit_closure(closure_id)?;

        // Materializing the already-resolved closure is part of committing a
        // usable result, not optional expansion. Skipping it at the deadline
        // leaves extracted symbols visible but their call graph empty.
        let stats = self
            .graph_builder
            .build_for_closure(closure_id, last_generation)?;
        if stats.candidate_count > 0 {
            self.store
                .make_candidate_edges_visible(closure_id, last_generation)?;
        }
        self.store
            .prune_committed_closures(RETAINED_COMMITTED_CLOSURES)?;
        tracing::debug!(
            closure_id = %closure_id,
            edges_built = stats.stats.edges_built,
            edges_written = stats.stats.edges_written,
            candidate_count = stats.candidate_count,
            "FocusGraphBuilder completed"
        );

        Ok(closure)
    }

    /// Trigger dataflow extraction from the query's exact Focus seed.
    ///
    /// Called after [`build_closure`] completes graph building at the
    /// structural level. Position seeds use
    /// [`LazyDataflowService::ensure_for_position_with_depth`]; symbol seeds
    /// resolve the matching callable(s) inside the closure and use
    /// [`LazyDataflowService::ensure_for_function_with_depth`]. Both existing
    /// ensure paths expand their own caller/callee dependency window, so
    /// dataflow breadth is query-driven rather than "every function in every
    /// hot file".
    ///
    /// Returns the number of functions for which dataflow was successfully
    /// planned and ensured.
    pub fn build_dataflow_for_closure(
        &self,
        closure_id: &str,
        seed: &FocusSeed,
        max_depth: usize,
        files: &HashSet<FileId>,
    ) -> Result<usize> {
        match seed {
            FocusSeed::Position {
                file_id,
                line,
                column,
            } => {
                if !files.contains(file_id) {
                    anyhow::bail!("dataflow position seed is outside the completed Focus closure");
                }
                let label = format!("{}:{line}:{column}", file_id.to_hex());
                Self::wait_for_dataflow(&label, || {
                    self.materialize.dataflow().ensure_for_position_with_depth(
                        file_id,
                        *line,
                        *column,
                        max_depth,
                        Some(closure_id),
                    )
                })?;
                Ok(1)
            }
            FocusSeed::Symbol { name, file_id, .. } => {
                let mut region_symbols = Vec::new();
                for candidate_file in files {
                    if file_id.is_some_and(|seed_file| seed_file != *candidate_file) {
                        continue;
                    }
                    region_symbols.extend(self.store.find_symbols_by_file(candidate_file)?);
                }
                let matched_seed = region_symbols
                    .iter()
                    .any(|symbol| symbol.name == *name || symbol.qualified_name == *name);
                let mut candidates: Vec<_> = region_symbols
                    .iter()
                    .filter(|symbol| {
                        matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                            && (symbol.name == *name || symbol.qualified_name == *name)
                    })
                    .cloned()
                    .collect();
                if candidates.is_empty() && matched_seed {
                    // Semantic impact may start from a type or field. Its
                    // callable dataflow region is still bounded by the
                    // query-driven structural closure.
                    candidates.extend(region_symbols.into_iter().filter(|symbol| {
                        matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                    }));
                }
                candidates.sort_by_key(|symbol| symbol.id.to_hex());
                candidates.dedup_by_key(|symbol| symbol.id);
                if candidates.is_empty() {
                    if matched_seed {
                        return Ok(0);
                    }
                    anyhow::bail!(
                        "no callable matching Focus seed '{name}' was available for dataflow"
                    );
                }

                for symbol in &candidates {
                    Self::wait_for_dataflow(&symbol.qualified_name, || {
                        self.materialize.dataflow().ensure_for_function_with_depth(
                            &symbol.id,
                            max_depth,
                            Some(closure_id),
                        )
                    })?;
                }
                Ok(candidates.len())
            }
            FocusSeed::File { file_id, .. } => {
                if !files.contains(file_id) {
                    return Ok(0);
                }
                let mut candidates: Vec<_> = self
                    .store
                    .find_symbols_by_file(file_id)?
                    .into_iter()
                    .filter(|symbol| {
                        matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                    })
                    .collect();
                candidates.sort_by_key(|symbol| symbol.id.to_hex());
                for symbol in &candidates {
                    Self::wait_for_dataflow(&symbol.qualified_name, || {
                        self.materialize.dataflow().ensure_for_function_with_depth(
                            &symbol.id,
                            max_depth,
                            Some(closure_id),
                        )
                    })?;
                }
                Ok(candidates.len())
            }
            FocusSeed::Field { .. } => {
                anyhow::bail!("Sync dataflow requires a callable, position, or file Focus seed")
            }
        }
    }

    fn wait_for_dataflow(
        label: &str,
        mut ensure: impl FnMut() -> Result<types::lazy::LazyWindow>,
    ) -> Result<()> {
        let wait_started = Instant::now();
        loop {
            let window = ensure().with_context(|| format!("dataflow build failed for {label}"))?;
            let complete = window
                .quality
                .as_ref()
                .is_some_and(|quality| quality.is_exact());
            if window.units_pending == 0 && complete {
                return Ok(());
            }
            if wait_started.elapsed() >= Duration::from_secs(60) {
                anyhow::bail!(
                    "dataflow for {label} remained incomplete for 60s ({} pending job(s), {}/{} units ready)",
                    window.units_pending,
                    window.units_built + window.units_cached,
                    window.units.len(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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
    ) -> Option<Box<VisibilityFilterFn>> {
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
            FocusSeed::Symbol { name, file_id, .. } => match file_id {
                Some(file_id) => Ok(vec![*file_id]),
                None => self
                    .materialize
                    .structural()
                    .candidate_files_for_symbol(name)
                    .map(|ids| ids.into_iter().take(5).collect())
                    .context("Failed to locate seed symbol"),
            },
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
    ) -> Result<(Vec<FileId>, Vec<SymbolId>)> {
        // Per-iteration call expansion is always depth-1; multi-hop is the
        // fixed-point loop (window.max_iterations / iterations_for).
        if depth != 1 {
            anyhow::bail!(
                "expand_callgraph only supports depth=1 (got {depth}); multi-hop is \
                 fixed-point iteration, not CallGraph.depth"
            );
        }

        let mut result: HashSet<FileId> = HashSet::new();
        let mut relevant_symbols: HashSet<SymbolId> = HashSet::new();

        let do_outgoing = matches!(direction, Direction::Outgoing | Direction::Both);
        let do_incoming = matches!(direction, Direction::Incoming | Direction::Both);

        if do_outgoing {
            for source_symbol in &closure.symbols {
                let targets = self.store.get_resolved_targets_for_symbol_in_closure(
                    closure_id,
                    source_symbol.as_bytes(),
                    ReferenceKind::Call.as_str(),
                )?;
                for target_blob in &targets {
                    if target_blob.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(target_blob);
                        let target_sym_id = SymbolId::from_bytes(arr);
                        relevant_symbols.insert(target_sym_id);
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
            if let FocusSeed::Symbol { name, .. } = &closure.seed {
                for file_id in self
                    .materialize
                    .structural()
                    .candidate_files_referencing(name)?
                {
                    if !closure.visited.contains(&file_id) {
                        result.insert(file_id);
                    }
                }
            }
            for target_symbol in &closure.symbols {
                let callers = self.store.get_callers_for_symbol_in_closure(
                    closure_id,
                    target_symbol.as_bytes(),
                    ReferenceKind::Call.as_str(),
                )?;
                for source_blob in &callers {
                    if source_blob.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(source_blob);
                        let source_sym_id = SymbolId::from_bytes(arr);
                        relevant_symbols.insert(source_sym_id);
                        if let Ok(Some(file_id)) = self.store.find_symbol_file(&source_sym_id) {
                            if !closure.visited.contains(&file_id) {
                                result.insert(file_id);
                            }
                        }
                    }
                }
            }
        }

        Ok((
            result.into_iter().collect(),
            relevant_symbols.into_iter().collect(),
        ))
    }

    /// Plan additions based on strategies + current closure state.
    fn plan_additions(
        &self,
        strategies: &[ClosureStrategy],
        closure: &FocusClosure,
        closure_id: &str,
        iteration: u32,
    ) -> Result<(Vec<FileId>, Vec<SymbolId>)> {
        let mut additions = Vec::new();
        let mut relevant_symbols = HashSet::new();

        for strategy in strategies {
            match strategy {
                ClosureStrategy::ImportNeighborhood { .. } => {}
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
                    let (callee_files, symbols) =
                        self.expand_callgraph(closure, closure_id, *direction, *depth)?;
                    additions.extend(callee_files);
                    relevant_symbols.extend(symbols);
                }
                ClosureStrategy::TypeGraph { max_depth } => {
                    if iteration <= *max_depth {
                        let (files, symbols) = self.expand_types(closure, 1, closure_id)?;
                        additions.extend(files);
                        relevant_symbols.extend(symbols);
                    }
                }
                ClosureStrategy::StateChannel => {
                    additions.extend(self.expand_state_channels(closure)?);
                }
            }
        }

        // Dedup without sorting: strategy order expresses semantic priority
        // (e.g. imports before call-graph expansion for C/C++ visibility).
        let mut seen = HashSet::new();
        additions.retain(|file_id| seen.insert(*file_id));
        additions.retain(|f| !closure.visited.contains(f));

        Ok((additions, relevant_symbols.into_iter().collect()))
    }

    fn expand_state_channels(&self, closure: &FocusClosure) -> Result<Vec<FileId>> {
        let mut has_arkts_reactive_state = false;
        for file_id in &closure.files {
            if self
                .store
                .get_file(file_id)?
                .is_none_or(|file| file.language != Language::ArkTS)
            {
                continue;
            }
            if self
                .store
                .find_references_by_file(file_id)?
                .iter()
                .any(|reference| {
                    reference.kind == ReferenceKind::Call
                        && matches!(reference.name.as_str(), "StorageLink" | "StorageProp")
                })
            {
                has_arkts_reactive_state = true;
                break;
            }
        }
        if !has_arkts_reactive_state {
            return Ok(Vec::new());
        }

        let structural = self.materialize.structural();
        let mut candidates = structural.candidate_files_referencing("AppStorage.setOrCreate")?;
        candidates.extend(structural.candidate_files_referencing("AppStorage.set")?);
        candidates.sort_by_key(FileId::to_hex);
        candidates.dedup();
        Ok(candidates)
    }

    /// Extract a single file using Focus structural materialize.
    fn extract_file_for_closure(
        &self,
        file_id: &FileId,
        budget: &LazyBudget,
    ) -> Result<EnsureStructuralResult> {
        let result = self
            .materialize
            .structural()
            .ensure_structural_for_file_in_closure(file_id, Some(budget))?;
        Ok(result)
    }

    fn materialize_import_dependencies(
        &self,
        window: &FocusWindow,
        closure: &FocusClosure,
        strategies: &[ClosureStrategy],
        closure_id: &str,
        generation: i64,
        max_files: usize,
    ) -> Result<Vec<KnownGap>> {
        let depth = strategies
            .iter()
            .filter_map(|strategy| match strategy {
                ClosureStrategy::ImportNeighborhood { depth } => Some(*depth as usize),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        if depth == 0 || closure.files.is_empty() {
            return Ok(Vec::new());
        }

        let planner = ClosurePlanner::new(self.store.clone())
            .with_include_roots(window.include_roots.clone())
            .with_limits(depth, max_files.max(1));
        let mut dependencies = HashSet::new();
        for file_id in &closure.files {
            dependencies.extend(planner.plan_dependencies(file_id)?);
        }
        dependencies.retain(|file_id| !closure.files.contains(file_id));
        let mut dependencies: Vec<_> = dependencies.into_iter().collect();
        dependencies.sort_by_key(FileId::to_hex);

        let truncated = dependencies.len().saturating_sub(max_files);
        dependencies.truncate(max_files);
        let result = self
            .materialize
            .structural()
            .ensure_resolution_symbols_for_file_ids_in_closure(&dependencies)?;
        for file_id in result
            .built_file_ids
            .iter()
            .chain(result.cached_file_ids.iter())
        {
            self.store.insert_closure_coverage(
                closure_id,
                file_id.as_bytes(),
                "extracted_resolution_symbols",
                generation,
                None,
            )?;
        }

        let mut gaps = Vec::new();
        for (file_id, reason) in result.failed_files {
            let file = self
                .store
                .get_file(&file_id)?
                .map(|info| info.path)
                .unwrap_or_else(|| file_id.to_hex());
            gaps.push(KnownGap::ExtractionFailed { file, reason });
        }
        if truncated > 0 || result.budget_exceeded {
            gaps.push(KnownGap::BudgetExhausted {
                strategy: "import_resolution_symbols".to_string(),
                remaining: truncated.max(usize::from(result.budget_exceeded)),
            });
        }
        Ok(gaps)
    }

    fn record_extraction_outcome(
        &self,
        closure: &mut FocusClosure,
        file_id: FileId,
        result: &EnsureStructuralResult,
    ) -> bool {
        let ready =
            result.built_file_ids.contains(&file_id) || result.cached_file_ids.contains(&file_id);
        if ready {
            closure.mark_extracted(file_id, &result.quality);
            return true;
        }

        closure.visited.insert(file_id);
        if result.files_pending > 0 {
            closure.record_pending_extraction_jobs(result.pending_job_ids.clone());
            return false;
        }
        if let Some((_, reason)) = result
            .failed_files
            .iter()
            .find(|(failed_id, _)| *failed_id == file_id)
        {
            let file = self
                .store
                .get_file(&file_id)
                .ok()
                .flatten()
                .map(|info| info.path)
                .unwrap_or_else(|| file_id.to_hex());
            closure.record_gap(KnownGap::ExtractionFailed {
                file,
                reason: reason.clone(),
            });
        } else {
            closure.record_gap(KnownGap::BudgetExhausted {
                strategy: "structural_extraction".to_string(),
                remaining: 1,
            });
        }
        false
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
    ) -> Result<(Vec<FileId>, Vec<SymbolId>)> {
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
        let mut all_symbols: Vec<SymbolId> = Vec::new();
        let mut current_symbols: HashSet<SymbolId> = closure.symbols.iter().copied().collect();

        for _depth in 0..max_depth {
            let mut depth_additions = Vec::new();
            let mut depth_symbols = Vec::new();

            for source_symbol in &current_symbols {
                let mut resolved_target_ids: HashSet<SymbolId> = HashSet::new();
                for kind in type_ref_kinds {
                    let closure_targets = self.store.get_resolved_targets_for_symbol_in_closure(
                        closure_id,
                        source_symbol.as_bytes(),
                        kind.as_str(),
                    )?;
                    for target_blob in &closure_targets {
                        if target_blob.len() == 32 {
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(target_blob);
                            resolved_target_ids.insert(SymbolId::from_bytes(arr));
                        }
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
                    if !closure.symbols.contains(target_id)
                        && !all_symbols.contains(target_id)
                        && !depth_symbols.contains(target_id)
                    {
                        depth_symbols.push(*target_id);
                    }
                }
            }

            if depth_additions.is_empty() && depth_symbols.is_empty() {
                break; // no more transitive type deps to discover
            }

            all_additions.extend(depth_additions.iter().copied());
            all_symbols.extend(depth_symbols.iter().copied());
            current_symbols = depth_symbols.into_iter().collect();
        }

        Ok((all_additions, all_symbols))
    }
}

fn symbol_matches_seed(seed: &FocusSeed, symbol: &SymbolDef) -> bool {
    match seed {
        FocusSeed::Symbol { name, kind, .. } => {
            (symbol.name == *name || symbol.qualified_name == *name)
                && kind.is_none_or(|kind| symbol.kind == kind)
        }
        _ => true,
    }
}
