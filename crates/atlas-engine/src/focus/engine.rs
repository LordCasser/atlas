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
//! - [`ClosureStrategy::CallGraph`] and [`ClosureStrategy::TypeGraph`] are
//!   stubs for this phase.
//!
//! # Visibility
//!
//! Built closures are committed atomically via [`Store::commit_closure_generation`]
//! and [`Store::make_coverage_visible`], which gates MCP query results.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use db::Store;
use resolution::ReferenceResolver;
use types::enums::{ReferenceKind, SymbolKind};
use types::ids::FileId;
use types::structs::KnownGap;

use crate::closure_planner::{ClosurePlanner, IncludeRoot};
use crate::focus::focus_graph_builder::FocusGraphBuilder;
use crate::lazy_budget::LazyBudget;
use crate::lazy_structural::{EnsureStructuralResult, LazyStructuralService};

use super::types::{
    ClosureStrategy, Direction, FocusClosure, FocusSeed, FocusWindow,
};

/// Engine for building focus closures around a user's seed.
///
/// Implements bounded fixed-point iteration: plan → extract → resolve → plan
/// until no additions, budget exhausted, or max_iterations reached.
pub struct ClosureEngine {
    pub(crate) store: Arc<Store>,
    pub(crate) lazy_structural: LazyStructuralService,
    pub(crate) resolver: RefCell<ReferenceResolver>,
    pub(crate) graph_builder: FocusGraphBuilder,
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
        let resolver = RefCell::new(ReferenceResolver::new(store.clone()));
        let graph_builder = FocusGraphBuilder::new(store.clone());
        ClosureEngine {
            store,
            lazy_structural,
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
                &format!("{:?}", result.precision_tier),
            )?;
        }

        // Phase 2: pre-compute TypeGraph expansion (single-pass, not iterative).
        // TypeGraph handles its own depth internally — we do NOT re-invoke it
        // inside the fixed-point loop below.
        let mut pre_additions = Vec::new();
        for strategy in &window.strategies {
            if let ClosureStrategy::TypeGraph { max_depth } = strategy {
                let type_files = self.expand_types(&closure, *max_depth)?;
                for f in type_files {
                    if !closure.visited.contains(&f) {
                        pre_additions.push(f);
                    }
                }
            }
        }

        // Phase 3: bounded fixed-point expansion
        let mut last_generation: i64 = 0; // generation 0 = seed extraction
        loop {
            iteration += 1;

            // Plan: ask strategies what to add next.
            // Merge pre-computed TypeGraph results into the first iteration.
            let additions = if iteration == 1 && !pre_additions.is_empty() {
                let mut plan = self.plan_additions(&window.strategies, &closure)?;
                for f in &pre_additions {
                    if !plan.contains(f) {
                        plan.push(*f);
                    }
                }
                plan
            } else {
                self.plan_additions(&window.strategies, &closure)?
            };

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
            last_generation = generation;
            for file_id in &new_files {
                let result = self.extract_file(file_id)?;
                closure.mark_extracted(*file_id, result.precision_tier);

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

        // Resolve references scoped to this closure (writes to reference_resolutions)
        let closure_files: Vec<FileId> = closure.files.iter().copied().collect();
        if !closure_files.is_empty() {
            self.resolver.borrow_mut().resolve_for_closure(
                closure_id,
                last_generation,
                &closure_files,
                None, // visibility_filter = None means all symbols visible (MVP)
            )?;
        }

        // Commit: atomic visibility switch
        self.commit_closure(closure_id, last_generation)?;

        // Build scoped graph edges from closure resolutions.
        // FocusGraphBuilder reads from reference_resolutions (is_visible=1)
        // and routes edges via EdgeConflictPolicy.
        let stats = self
            .graph_builder
            .build_for_closure(closure_id, last_generation)?;
        tracing::debug!(
            closure_id = %closure_id,
            edges_built = stats.stats.edges_built,
            edges_written = stats.stats.edges_written,
            candidate_count = stats.candidate_count,
            "FocusGraphBuilder completed"
        );

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

    /// Expand closure through call-graph edges.
    ///
    /// For `Direction::Outgoing`: queries canonical edges (`symbol_edges`) and
    /// visible candidate edges (`symbol_edge_candidates`) for each symbol in
    /// the closure, resolves **target** symbols to their containing files, and
    /// returns deduplicated file IDs not already in `closure.visited`.
    ///
    /// For `Direction::Incoming`: queries canonical and candidate edges where
    /// the closure symbol is the **target** (i.e. "who calls us?"), resolves
    /// the edge's **source** symbol to its file, and returns those files.
    ///
    /// For `Direction::Both`: combines Outgoing + Incoming results.
    ///
    /// `depth != 1` returns empty because multi-hop expansion is deferred to
    /// fixed-point iteration.
    fn expand_callgraph(
        &self,
        closure: &FocusClosure,
        direction: Direction,
        depth: u32,
    ) -> Result<Vec<FileId>> {
        // Only single-level expansion is supported; multi-hop is handled by
        // the fixed-point loop (each iteration re-queries with newly extracted
        // symbols).
        if depth != 1 {
            return Ok(Vec::new());
        }

        match direction {
            Direction::Incoming => {
                let mut source_files: Vec<FileId> = Vec::new();

                for symbol_id in &closure.symbols {
                    // Canonical incoming edges (symbol_edges table)
                    if let Ok(edges) = self.store.find_edges_by_target(symbol_id) {
                        for edge in &edges {
                            if let Ok(Some(file_id)) =
                                self.store.find_symbol_file(&edge.source)
                            {
                                if !closure.visited.contains(&file_id) {
                                    source_files.push(file_id);
                                }
                            }
                        }
                    }

                    // Candidate incoming edges (symbol_edge_candidates table)
                    if let Ok(candidates) =
                        self.store.find_visible_candidate_edges_by_target(symbol_id)
                    {
                        for cand in &candidates {
                            let source_blob = &cand.source;
                            if source_blob.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(source_blob);
                                let sym_id =
                                    types::ids::SymbolId::from_bytes(arr);
                                if let Ok(Some(file_id)) =
                                    self.store.find_symbol_file(&sym_id)
                                {
                                    if !closure.visited.contains(&file_id) {
                                        source_files.push(file_id);
                                    }
                                }
                            }
                        }
                    }
                }

                source_files.sort();
                source_files.dedup();
                Ok(source_files)
            }
            Direction::Outgoing => {
                let mut target_files: Vec<FileId> = Vec::new();

                for symbol_id in &closure.symbols {
                    // Canonical edges (symbol_edges table)
                    if let Ok(edges) = self.store.find_edges_by_source(symbol_id) {
                        for edge in &edges {
                            if let Ok(Some(file_id)) =
                                self.store.find_symbol_file(&edge.target)
                            {
                                if !closure.visited.contains(&file_id) {
                                    target_files.push(file_id);
                                }
                            }
                        }
                    }

                    // Candidate edges (symbol_edge_candidates table)
                    if let Ok(candidates) =
                        self.store.find_visible_candidate_edges_by_source(symbol_id)
                    {
                        for cand in &candidates {
                            if let Some(ref target_blob) = cand.target {
                                if target_blob.len() == 32 {
                                    let mut arr = [0u8; 32];
                                    arr.copy_from_slice(target_blob);
                                    let sym_id =
                                        types::ids::SymbolId::from_bytes(arr);
                                    if let Ok(Some(file_id)) =
                                        self.store.find_symbol_file(&sym_id)
                                    {
                                        if !closure.visited.contains(&file_id) {
                                            target_files.push(file_id);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                target_files.sort();
                target_files.dedup();
                Ok(target_files)
            }
            Direction::Both => {
                // Combine Outgoing (callee files) and Incoming (caller files).
                let mut outgoing = self.expand_callgraph(
                    closure,
                    Direction::Outgoing,
                    depth,
                )?;
                let mut incoming = self.expand_callgraph(
                    closure,
                    Direction::Incoming,
                    depth,
                )?;
                outgoing.append(&mut incoming);
                outgoing.sort();
                outgoing.dedup();
                Ok(outgoing)
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
                ClosureStrategy::CallGraph { direction, depth } => {
                    let callee_files =
                        self.expand_callgraph(closure, *direction, *depth)?;
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
    fn commit_closure(&self, closure_id: &str, resolution_generation: i64) -> Result<i64> {
        // P1a fix: make ALL staged rows visible regardless of generation.
        // Seed files are written with generation=0, expansion files with
        // generation=iteration. We must flip all of them, not just the
        // committed generation number.
        let generation = self.store.commit_closure_generation(closure_id)?;
        self.store.make_all_staged_coverage_visible(closure_id)?;
        // T6/D: make scoped reference resolutions visible too
        self.store.make_resolutions_visible(closure_id, resolution_generation)?;
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
    /// For each file in scope, find references whose resolved target is a
    /// type definition (Struct/Class/Enum/Interface/Trait).  Add the file
    /// containing that type definition to the closure.  When `max_depth > 1`,
    /// repeat the process for the newly added files to discover transitive
    /// type dependencies.
    fn expand_types(
        &self,
        closure: &FocusClosure,
        max_depth: u32,
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
                let refs = self.store.find_references_by_file_and_kinds(
                    file_id,
                    &type_ref_kinds,
                )?;

                for r in &refs {
                    let target_id = match &r.resolved {
                        Some(resolved) => &resolved.symbol_id,
                        None => continue,
                    };

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
