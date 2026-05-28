//! Resolution layer: import resolver, name matcher, framework resolvers.
//!
//! Three-stage reference resolution pipeline:
//! 1. Scope-local exact match
//! 2. Import / include resolution
//! 3. Name-based fuzzy fallback (project-wide)
//!
//! P2: Resolver only produces resolved facts — `Vec<(ReferenceUse, ResolvedTarget)>`.
//! Edge creation is handled by `GraphBuilder` in the `graph` module.
//!
//! Cross-module invariant: references are NEVER deleted — resolution updates
//! their `resolved` field in place but leaves the record intact.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use db::Store;
use rayon::prelude::*;
use types::*;
use types::progress::ProgressPhase;

use self::builtins::BuiltinFilter;
use self::context::{GlobalSymbolIndex, ResolutionContext};
use self::import_resolver::ImportResolver;
use self::name_matcher::NameMatcher;

pub mod builtins;
pub mod config;
pub mod context;
pub mod frameworks;
pub mod import_resolver;
pub mod include_graph;
pub mod name_matcher;
pub mod path_alias;

pub use config::{commit_config_hashes, detect_config_change};
pub use include_graph::IncludeGraph;
pub use path_alias::PathAliasResolver;

// ── Shared resolution core ─────────────────────────────────────────────────
//
// Both ResolutionSession and ReferenceResolver need the same 6-strategy
// resolution pipeline.  The only behavioral difference is Strategy 6
// (project-wide name search): one uses file-proximity scoring, the other
// uses plain name lookup.  Parameterizing that difference lets both structs
// share a single implementation.

/// The 6-strategy resolution core — pure function, no I/O.
///
/// `global_index` and `proximity_file_id` control Strategy 6:
/// - `Some(idx)` + `Some(fid)`: proximity-scored search (ResolutionSession)
/// - `Some(idx)` + `None`: plain name search (ReferenceResolver)
/// - `None`: skip Strategy 6 entirely (global_index not built yet)
fn resolve_one_core(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    import_resolver: &ImportResolver,
    name_matcher: &NameMatcher,
    global_index: Option<&GlobalSymbolIndex>,
    proximity_file_id: Option<FileId>,
) -> Option<ResolvedTarget> {
    // Strategy 1: Built-in / external filter
    if BuiltinFilter::is_builtin(&reference.name, ctx.file.language) {
        return None;
    }

    // Strategy 2: Scope-local exact match
    if let Some(scope_id) = reference.scope_id {
        if let Some(sym) = ctx.lookup_scoped(scope_id, &reference.name) {
            return Some(ResolvedTarget {
                symbol_id: sym.id,
                confidence: Confidence::certain(),
                strategy: ResolutionStrategy::ExactMatch,
                provenance: Provenance::TreeSitter,
            });
        }
    }

    // Strategy 3: Container/class-local
    if let Some(source_sym) = reference.source_symbol {
        if let Some(source) = ctx.symbols_by_id.get(&source_sym) {
            // If the source symbol is a method, look for the target in its containing class
            if let Some(container) = source.container {
                if let Some(container_sym) = ctx.symbols_by_id.get(&container) {
                    if let Some(scope) = container_sym.scope_id {
                        if let Some(sym) = ctx.lookup_scoped(scope, &reference.name) {
                            return Some(ResolvedTarget {
                                symbol_id: sym.id,
                                confidence: Confidence::certain(),
                                strategy: ResolutionStrategy::ExactMatch,
                                provenance: Provenance::TreeSitter,
                            });
                        }
                    }
                }
            }
        }
    }

    // Strategy 4: Same-file exact match
    let same_file = ctx.find_in_file_by_name(&reference.name);
    if let Some(matched) = name_matcher.best_match(
        &same_file,
        &reference.name,
        Confidence::certain(),
    ) {
        return Some(ResolvedTarget {
            symbol_id: matched.symbol_id,
            confidence: matched.confidence,
            strategy: matched.strategy,
            provenance: matched.provenance,
        });
    }

    // Strategy 5: Import/include resolution
    //
    // Uses the pre-built imports_by_name index for O(1) lookup instead of
    // iterating all imports per reference.  The index maps import imported_name
    // and local_name (alias) to indices into ctx.imports.
    if let Some(import_indices) = ctx.imports_by_name.get(&reference.name) {
        for &idx in import_indices {
            let import = &ctx.imports[idx];
            let import_local = import.local_name.as_deref().unwrap_or("");
            let matches_by_alias = !import_local.is_empty() && import_local == reference.name;

            if let Ok(candidates) = import_resolver.resolve_import(import) {
                if let Ok(chain_candidates) = import_resolver
                    .resolve_through_reexports(import, candidates)
                {
                    // Alias match: trust the import relationship directly.
                    if matches_by_alias {
                        if let Some(first) = chain_candidates.first() {
                            return Some(ResolvedTarget {
                                symbol_id: first.id,
                                confidence: Confidence::new(0.8),
                                strategy: ResolutionStrategy::ImportResolved,
                                provenance: Provenance::Heuristic,
                            });
                        }
                    }
                    // Exact-name match: use name_matcher to filter candidates.
                    // (The index only contains imports whose imported_name or
                    // local_name equals reference.name, so we always match.)
                    if let Some(matched) = name_matcher.best_match(
                        &chain_candidates,
                        &reference.name,
                        Confidence::certain(),
                    ) {
                        return Some(ResolvedTarget {
                            symbol_id: matched.symbol_id,
                            confidence: Confidence::new(0.8),
                            strategy: ResolutionStrategy::ImportResolved,
                            provenance: Provenance::Heuristic,
                        });
                    }
                }
            }
        }
    }

    // Strategy 6: Project-wide name search + fuzzy fallback
    if let Some(idx) = global_index {
        let candidates = match proximity_file_id {
            Some(fid) => idx.find_by_name_proximity(&reference.name, fid),
            None => idx.find_by_name(&reference.name),
        };
        if !candidates.is_empty() {
            if let Some(matched) = name_matcher.best_match(
                &candidates,
                &reference.name,
                Confidence::new(0.6),
            ) {
                return Some(ResolvedTarget {
                    symbol_id: matched.symbol_id,
                    confidence: matched.confidence,
                    strategy: ResolutionStrategy::FuzzyMatch,
                    provenance: Provenance::Heuristic,
                });
            }
        }
        let fuzzy = idx.fuzzy_search(&reference.name, 2);
        if !fuzzy.is_empty() {
            if let Some(matched) = name_matcher.best_match(
                &fuzzy,
                &reference.name,
                Confidence::new(0.4),
            ) {
                return Some(ResolvedTarget {
                    symbol_id: matched.symbol_id,
                    confidence: matched.confidence,
                    strategy: ResolutionStrategy::FuzzyMatch,
                    provenance: Provenance::Heuristic,
                });
            }
        }
    }

    None
}

// ── ResolutionSession ──────────────────────────────────────────────────────
//
// A thread-safe, read-only resolution context that can be shared across
// rayon threads during parallel resolution.  All fields are Arc'd so the
// session implements Send + Sync.

/// Thread-safe resolution session for parallel reference resolution.
///
/// Built once from the `Store`, then shared across rayon threads.
/// Each thread builds a per-file `ResolutionContext` (brief read-lock)
/// and resolves all of that file's references in pure memory (no locks).
pub struct ResolutionSession {
    pub global_index: Arc<GlobalSymbolIndex>,
    pub import_resolver: Arc<ImportResolver>,
    pub name_matcher: Arc<NameMatcher>,
}

impl ResolutionSession {
    /// Build the session from the store.  Loads the global symbol index
    /// once — this is the most expensive single operation in resolution
    /// and only needs to happen once regardless of thread count.
    pub fn build(store: Arc<Store>) -> anyhow::Result<Self> {
        Ok(Self {
            global_index: Arc::new(GlobalSymbolIndex::build(&store)?),
            import_resolver: Arc::new(ImportResolver::new(store.clone())),
            name_matcher: Arc::new(NameMatcher::new()),
        })
    }

    /// Build with path alias support.
    pub fn build_with_alias(
        store: Arc<Store>,
        path_alias: PathAliasResolver,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            global_index: Arc::new(GlobalSymbolIndex::build(&store)?),
            import_resolver: Arc::new(ImportResolver::with_path_alias(
                store.clone(),
                path_alias,
            )),
            name_matcher: Arc::new(NameMatcher::new()),
        })
    }

    // ── Thread-safe resolution ───────────────────────────────────────────

    /// Resolve all references in a single file.
    ///
    /// Callable from multiple rayon threads simultaneously:
    /// 1. Briefly locks `store.read_conn` to build `ResolutionContext` (< 1 ms).
    /// 2. Resolves each reference in pure memory (no locks, no DB access).
    ///
    /// Returns `(reference, target)` pairs for resolved references.
    /// Unresolved references are silently dropped.
    pub fn resolve_file(
        &self,
        store: &Store,
        refs: &[(FileId, Vec<ReferenceUse>)],  // Single-element batch for this file
    ) -> anyhow::Result<Vec<(ReferenceUse, ResolvedTarget)>> {
        let mut results = Vec::new();
        for (file_id, references) in refs {
            let ctx = ResolutionContext::build(store, *file_id)?;
            for reference in references {
                if let Some(target) = self.resolve_one(reference, &ctx) {
                    results.push((reference.clone(), target));
                }
            }
        }
        Ok(results)
    }

    /// Resolve a batch of references with a pre-built context — pure memory,
    /// no Store access.  Designed for pre-loaded contexts in parallel
    /// resolution to eliminate lock contention on the single Store mutex.
    fn resolve_refs_in_ctx(
        &self,
        references: &[ReferenceUse],
        ctx: &ResolutionContext,
    ) -> Vec<(ReferenceUse, ResolvedTarget)> {
        let mut results = Vec::with_capacity(references.len());
        for reference in references {
            if let Some(target) = self.resolve_one(reference, ctx) {
                results.push((reference.clone(), target));
            }
        }
        results
    }

    /// Resolve one reference using the shared core with file-proximity scoring.
    fn resolve_one(
        &self,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
    ) -> Option<ResolvedTarget> {
        resolve_one_core(
            reference,
            ctx,
            &self.import_resolver,
            &self.name_matcher,
            Some(&self.global_index),
            Some(ctx.file.file_id),
        )
    }
}

// ── ReferenceResolver ──────────────────────────────────────────────────────

/// Three-stage reference resolution orchestrator.
///
/// P2: `resolve_all()` only resolves references and updates the `"references"`
/// table. Edge creation is delegated to `GraphBuilder`.
///
/// P4: Uses `GlobalSymbolIndex` for project-wide name search instead of
/// per-reference FTS5 queries. The global index is built once at the start
/// of resolution.
///
/// P6: `resolve_all_parallel()` uses rayon to parallelize per-file resolution,
/// with a Phase-1 (parallel matching) → Phase-2 (serial write) model.
pub struct ReferenceResolver {
    store: Arc<Store>,
    import_resolver: ImportResolver,
    name_matcher: NameMatcher,
    /// P4: Global in-memory symbol index (built once per resolve_all).
    global_index: Option<GlobalSymbolIndex>,
}

impl ReferenceResolver {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            import_resolver: ImportResolver::new(store.clone()),
            name_matcher: NameMatcher::new(),
            store,
            global_index: None,
        }
    }

    /// Create a ReferenceResolver with tsconfig.json path alias support.
    pub fn with_path_alias(store: Arc<Store>, path_alias: PathAliasResolver) -> Self {
        Self {
            import_resolver: ImportResolver::with_path_alias(store.clone(), path_alias),
            name_matcher: NameMatcher::new(),
            store,
            global_index: None,
        }
    }

    /// Resolve all unresolved references in the project (serial — kept for
    /// backwards compatibility and small projects).
    pub fn resolve_all(
        &mut self,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        // P4: Build global index once
        if self.global_index.is_none() {
            self.global_index = Some(GlobalSymbolIndex::build(&self.store)?);
        }
        let unresolved = self.store.find_unresolved_references()?;
        let total_refs = unresolved.len();
        let mut stats = ResolutionStats::default();
        stats.total_refs = total_refs;

        // Group references by file for efficient context loading
        let mut by_file: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
        for r in &unresolved {
            by_file.entry(r.file_id).or_default().push(r.clone());
        }

        // Accumulate resolutions for batched writes
        let mut pending_resolutions: Vec<(ReferenceId, ResolvedTarget)> = Vec::new();
        let mut all_resolved: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
        let batch_size = 500; // Flush every N resolutions

        for (file_id, refs) in &by_file {
            let ctx = match ResolutionContext::build(&self.store, *file_id) {
                Ok(c) => c,
                Err(e) => {
                    stats.add_warning(format!("failed to build context: {}", e));
                    continue;
                }
            };

            for reference in refs {
                match self.resolve_one(reference, &ctx) {
                    Some(target) => {
                        pending_resolutions.push((reference.id, target.clone()));
                        all_resolved.push((reference.clone(), target.clone()));
                        stats.resolved += 1;
                        *stats
                            .by_strategy
                            .entry(target.strategy.as_str().to_string())
                            .or_default() += 1;
                    }
                    None => {
                        stats.unresolved += 1;
                    }
                }
            }

            if pending_resolutions.len() >= batch_size {
                self.flush_resolutions(&mut pending_resolutions, &mut stats);
            }
        }

        self.flush_resolutions(&mut pending_resolutions, &mut stats);

        Ok((all_resolved, stats))
    }

    // ── Parallel resolution (P6) ───────────────────────────────────────────

    /// Resolve all unresolved references in parallel.
    ///
    /// **Phase 1** (rayon parallel): per-file resolution using
    /// `ResolutionSession` — shared read-only context, each thread
    /// briefly locks the read connection to build a `ResolutionContext`,
    /// then resolves references in pure memory.
    ///
    /// **Phase 2** (serial write): batch-update resolved references
    /// to the database, reporting progress after each batch.
    ///
    /// `on_progress` is called during Phase 2 with `(current, total)`
    /// after each batch write.  `progress_state` (if provided) is updated
    /// during Phase 1 using lock-free AtomicU64 increments.
    pub fn resolve_all_parallel(
        &mut self,
        store: Arc<Store>,
        progress_mutex: Option<&std::sync::Arc<std::sync::Mutex<types::progress::ProgressState>>>,
        on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        if let Some(mutex) = progress_mutex {
            mutex.lock().unwrap().start_phase(ProgressPhase::Resolution, None);
        }

        // Build shared session
        let session = ResolutionSession::build(store.clone())?;

        // Load all unresolved references, grouped by file
        let unresolved = store.find_unresolved_references()?;
        let total_refs = unresolved.len() as u64;
        let by_file: Vec<(FileId, Vec<ReferenceUse>)> = {
            let mut map: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
            for r in &unresolved {
                map.entry(r.file_id).or_default().push(r.clone());
            }
            map.into_iter().collect()
        };

        // ── Phase 1: Parallel resolution (reads only) ──
        //
        // Step A (serial, 1 lock): Pre-build ResolutionContext for every file
        // so that rayon threads never touch the Store mutex.  This eliminates
        // the primary bottleneck on large projects: 8+ threads serializing on
        // a single SQLite connection for 4 queries per file.
        //
        // Step B (parallel, lock-free): Each thread resolves its file's refs
        // against its pre-built context — pure memory, zero contention.
        let matched_counter = Arc::new(AtomicU64::new(0));
        let mc = &matched_counter;
        let session = &session;

        let progress_atomic = progress_mutex
            .map(|a| Arc::clone(&a.lock().unwrap().atomic_current));

        // Step A: build all contexts (one lock acquisition via the Store's
        // internal get_file / find_symbols_by_file etc.).
        let mut file_groups: Vec<(FileId, Vec<ReferenceUse>, ResolutionContext)> =
            Vec::with_capacity(by_file.len());
        for (fid, refs) in by_file {
            match ResolutionContext::build(&store, fid) {
                Ok(ctx) => file_groups.push((fid, refs, ctx)),
                Err(_e) => { /* skip — context build failed, refs remain unresolved */ }
            }
        }

        // Step B: pure-memory parallel resolution
        let per_file_results: Vec<(ReferenceUse, ResolvedTarget)> = file_groups
            .par_iter()
            .map(|(_fid, refs, ctx)| {
                let result = session.resolve_refs_in_ctx(refs, ctx);
                let count = result.len() as u64;
                let total_matched = mc.fetch_add(count, Ordering::Relaxed) + count;
                if let Some(ref ac) = progress_atomic {
                    ac.store(total_matched, Ordering::Relaxed);
                }
                result
            })
            .flatten()
            .collect();

        // Update progress state with Phase 1 result (for TUI to display the
        // matched count + rate before Phase 2 begins).
        if let Some(mutex) = progress_mutex {
            let matched = matched_counter.load(Ordering::Relaxed);
            mutex.lock().unwrap().set_current(matched);
        }

        // ── Phase 2: Serial write + progress ──
        if let Some(mutex) = progress_mutex {
            mutex.lock().unwrap().enter_phase2(total_refs);
        }

        let mut stats = ResolutionStats::default();
        stats.total_refs = total_refs as usize;

        let mut pending: Vec<(ReferenceId, ResolvedTarget)> = Vec::new();
        let all_resolved = per_file_results; // moved in
        let mut processed = 0u64;
        let batch_size = 2000;

        for (reference, target) in &all_resolved {
            pending.push((reference.id, target.clone()));
            processed += 1;
            stats.resolved += 1;
            *stats
                .by_strategy
                .entry(target.strategy.as_str().to_string())
                .or_default() += 1;

            if pending.len() >= batch_size {
                store.batch_update_resolutions(&pending)?;
                pending.clear();

                if let Some(cb) = on_progress {
                    cb(processed, total_refs);
                }
                if let Some(mutex) = progress_mutex {
                    mutex.lock().unwrap().set_current(processed);
                }
            }
        }

        // Final flush
        if !pending.is_empty() {
            store.batch_update_resolutions(&pending)?;
            if let Some(cb) = on_progress {
                cb(processed, total_refs);
            }
            if let Some(mutex) = progress_mutex {
                mutex.lock().unwrap().set_current(processed);
            }
        }

        stats.unresolved = total_refs as usize - stats.resolved;

        Ok((all_resolved, stats))
    }

    /// Wrapper that builds the session internally (convenience when you
    /// already have a `ReferenceResolver` and just want parallelism).
    pub fn resolve_all_parallel_simple(
        &mut self,
        progress_mutex: Option<&std::sync::Arc<std::sync::Mutex<types::progress::ProgressState>>>,
        on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        let store = self.store.clone();
        self.resolve_all_parallel(store, progress_mutex, on_progress)
    }

    /// Resolve references scoped to a specific set of files (lazy structural).
    ///
    /// Unlike [`resolve_all`], this does not scan every file — it only
    /// processes unresolved references that belong to `file_ids`.  References
    /// are grouped by file so that [`ResolutionContext`] is built once per
    /// file (same optimization as [`resolve_all`]).
    pub fn resolve_for_files(
        &mut self,
        file_ids: &[FileId],
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        if self.global_index.is_none() {
            self.global_index = Some(GlobalSymbolIndex::build(&self.store)?);
        }

        // Collect references from all target files, grouped by file_id
        let mut by_file: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
        for fid in file_ids {
            for r in self.store.find_references_by_file(fid)? {
                by_file.entry(r.file_id).or_default().push(r);
            }
        }

        let total_refs: usize = by_file.values().map(|v| v.len()).sum();
        let mut stats = ResolutionStats::default();
        stats.total_refs = total_refs;

        let mut pending_resolutions: Vec<(ReferenceId, ResolvedTarget)> = Vec::new();
        let mut all_resolved: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
        let batch_size = 500;

        for (file_id, refs) in &by_file {
            let ctx = match ResolutionContext::build(&self.store, *file_id) {
                Ok(c) => c,
                Err(e) => {
                    stats.add_warning(format!("failed to build context: {}", e));
                    continue;
                }
            };

            for reference in refs {
                match self.resolve_one(reference, &ctx) {
                    Some(target) => {
                        pending_resolutions.push((reference.id, target.clone()));
                        all_resolved.push((reference.clone(), target.clone()));
                        stats.resolved += 1;
                        *stats
                            .by_strategy
                            .entry(target.strategy.as_str().to_string())
                            .or_default() += 1;
                    }
                    None => {
                        stats.unresolved += 1;
                    }
                }
            }

            if pending_resolutions.len() >= batch_size {
                self.flush_resolutions(&mut pending_resolutions, &mut stats);
            }
        }

        self.flush_resolutions(&mut pending_resolutions, &mut stats);
        Ok((all_resolved, stats))
    }

    /// Flush pending resolution updates to the store in batch.
    fn flush_resolutions(
        &self,
        pending_resolutions: &mut Vec<(ReferenceId, ResolvedTarget)>,
        stats: &mut ResolutionStats,
    ) {
        if pending_resolutions.is_empty() {
            return;
        }
        if let Err(e) = self.store.batch_update_resolutions(pending_resolutions) {
            stats.add_warning(format!("batch resolution update failed: {}", e));
        }
        pending_resolutions.clear();
    }

    /// Resolve a single reference. Returns `None` if no match found.
    fn resolve_one(
        &self,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
    ) -> Option<ResolvedTarget> {
        resolve_one_core(
            reference,
            ctx,
            &self.import_resolver,
            &self.name_matcher,
            self.global_index.as_ref(),
            None,
        )
    }
}

/// Statistics from a resolution run.
#[derive(Debug, Clone, Default)]
pub struct ResolutionStats {
    pub total_refs: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub by_strategy: HashMap<String, usize>,
    /// Non-fatal warnings collected during resolution (context build failures,
    /// resolution update errors, etc.).
    pub warnings: Vec<String>,
}

impl ResolutionStats {
    /// Record a non-fatal warning.
    fn add_warning(&mut self, msg: String) {
        self.warnings.push(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use extraction::create_frontend;
    use extraction::extract_file;
    use graph::{GraphBuilder, GraphEngine};
    use std::path::PathBuf;

    /// Verifies that cross-file import → call creates a structural Calls edge
    /// through the Resolver + GraphBuilder pipeline.
    #[test]
    fn test_cross_file_import_call_creates_edge() {
        // ── File 1: lib.ts — exports a function ──
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let ts_frontend = create_frontend(Language::TypeScript).unwrap();
        let lib_facts = extract_file(
            &ts_frontend,
            lib_id,
            &PathBuf::from("lib.ts"),
            lib_src,
            "abc",
        )
        .expect("lib.ts extraction failed");

        // ── File 2: main.ts — imports and calls greet inside a function ──
        let main_src = r#"import { greet } from './lib';

function main() {
    const msg = greet("World");
    console.log(msg);
}
main();
"#;
        let main_id = FileId::generate("main.ts");
        let main_facts = extract_file(
            &ts_frontend,
            main_id,
            &PathBuf::from("main.ts"),
            main_src,
            "abc",
        )
        .expect("main.ts extraction failed");

        // ── Store and index ──
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store
            .insert_file_facts(&main_facts)
            .expect("insert main.ts");

        // ── Resolve ──
        let mut resolver = ReferenceResolver::new(Arc::clone(&store));
        let (resolved, stats) = resolver.resolve_all().expect("resolution failed");

        // Verify resolution happened
        assert!(
            stats.resolved > 0,
            "expected at least 1 resolved reference, got {}",
            stats.resolved
        );

        // ── Build edges ──
        let builder = GraphBuilder::new(Arc::clone(&store));
        let build_stats = builder.build_all(&resolved);
        assert!(
            build_stats.edges_built > 0,
            "expected cross-file Calls edges, got {} edges",
            build_stats.edges_built
        );
    }

    #[test]
    fn test_cross_file_callers_callees_graph() {
        // ── File 1: lib.ts — exports greet and farewell ──
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}

export function farewell(name: string): string {
    return `Goodbye, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let ts_frontend = create_frontend(Language::TypeScript).unwrap();
        let lib_facts = extract_file(
            &ts_frontend,
            lib_id,
            &PathBuf::from("lib.ts"),
            lib_src,
            "abc",
        )
        .expect("lib.ts extraction failed");

        // ── File 2: main.ts — imports and calls both functions ──
        let main_src = r#"import { greet, farewell } from './lib';

function main() {
    greet("World");
}

function shutdown() {
    farewell("World");
}

main();
shutdown();
"#;
        let main_id = FileId::generate("main.ts");
        let main_facts = extract_file(
            &ts_frontend,
            main_id,
            &PathBuf::from("main.ts"),
            main_src,
            "abc",
        )
        .expect("main.ts extraction failed");

        // ── Store, index, resolve ──
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store
            .insert_file_facts(&main_facts)
            .expect("insert main.ts");

        let mut resolver = ReferenceResolver::new(Arc::clone(&store));
        let (resolved, _) = resolver.resolve_all().expect("resolution failed");

        // ── Build edges ──
        let builder = GraphBuilder::new(Arc::clone(&store));
        builder.build_all(&resolved);

        // ── Build graph and verify callers/callees ──
        let graph = GraphEngine::from_store(&store, 0.0).expect("graph build failed");

        // Find greet and main by qualified name
        let greet_id = store
            .find_symbols_by_qname("greet")
            .unwrap()
            .first()
            .unwrap()
            .id;
        let main_id = store
            .find_symbols_by_qname("main")
            .unwrap()
            .first()
            .unwrap()
            .id;

        // Verify callers: main → greet
        let greet_callers = graph.callers(&greet_id);
        let caller_names: Vec<&str> = greet_callers
            .callers
            .iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            caller_names.contains(&"main"),
            "expected main to be caller of greet, got: {:?}",
            caller_names
        );

        // Verify callees: main → greet
        let main_callees = graph.callees(&main_id);
        let callee_names: Vec<&str> = main_callees
            .callees
            .iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            callee_names.contains(&"greet"),
            "expected greet to be callee of main, got: {:?}",
            callee_names
        );
    }

    /// Reproduce suspected bug: aliased import `import { foo as bar }`
    /// should still resolve `bar()` → Calls → `foo`.
    #[test]
    fn test_aliased_import_resolves_to_correct_symbol() {
        // ── File 1: lib.ts — exports a function ──
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let ts_frontend = create_frontend(Language::TypeScript).unwrap();
        let lib_facts = extract_file(
            &ts_frontend,
            lib_id,
            &PathBuf::from("lib.ts"),
            lib_src,
            "abc",
        )
        .expect("lib.ts extraction failed");

        // ── File 2: main.ts — aliased import + call ──
        let main_src = r#"import { greet as hello } from './lib';

function main() {
    hello("World");
}
main();
"#;
        let main_id = FileId::generate("main.ts");
        let main_facts = extract_file(
            &ts_frontend,
            main_id,
            &PathBuf::from("main.ts"),
            main_src,
            "abc",
        )
        .expect("main.ts extraction failed");

        // ── Store and resolve ──
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store.insert_file_facts(&main_facts).expect("insert main.ts");

        let mut resolver = ReferenceResolver::new(Arc::clone(&store));
        let (resolved, _stats) = resolver.resolve_all().expect("resolution failed");

        // Build edges
        let builder = GraphBuilder::new(Arc::clone(&store));
        builder.build_all(&resolved);

        // Build graph and verify
        let graph = GraphEngine::from_store(&store, 0.0).expect("graph build failed");

        let greet_id = store
            .find_symbols_by_qname("greet")
            .unwrap()
            .first()
            .unwrap()
            .id;

        let main_id = store
            .find_symbols_by_qname("main")
            .unwrap()
            .first()
            .unwrap()
            .id;

        // Verify: main → greet via aliased import
        let main_callees = graph.callees(&main_id);
        let callee_names: Vec<&str> = main_callees
            .callees
            .iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            callee_names.contains(&"greet"),
            "expected greet to be callee of main (via aliased import 'hello'), got: {:?}",
            callee_names
        );

        // Verify callers: greet is called by main
        let greet_callers = graph.callers(&greet_id);
        let caller_names: Vec<&str> = greet_callers
            .callers
            .iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            caller_names.contains(&"main"),
            "expected main to be caller of greet (via aliased import 'hello'), got: {:?}",
            caller_names
        );
    }
}
