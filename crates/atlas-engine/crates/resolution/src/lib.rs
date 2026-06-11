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
use std::sync::mpsc;

use db::Store;
use rayon::prelude::*;
use types::progress::ProgressPhase;
use types::*;

use self::builtins::BuiltinFilter;
use self::context::{GlobalSymbolIndex, ResolutionContext};
use self::import_resolver::ImportResolver;
use self::name_matcher::NameMatcher;

pub mod builtins;
pub mod config;

// Per-strategy hit counters (zero overhead: AtomicU64 inc is lock-free)
static S1_COUNT: AtomicU64 = AtomicU64::new(0);
static S2_COUNT: AtomicU64 = AtomicU64::new(0);
static S3_COUNT: AtomicU64 = AtomicU64::new(0);
static S4_COUNT: AtomicU64 = AtomicU64::new(0);
static S5_COUNT: AtomicU64 = AtomicU64::new(0);
static S6_COUNT: AtomicU64 = AtomicU64::new(0);
static S6_EXACT_COUNT: AtomicU64 = AtomicU64::new(0);
static S6_FUZZY_PROX_COUNT: AtomicU64 = AtomicU64::new(0);
static S6_FUZZY_GLOBAL_COUNT: AtomicU64 = AtomicU64::new(0);
static MISS_COUNT: AtomicU64 = AtomicU64::new(0);

// Per-strategy cumulative time counters (nanosecond resolution)
static S1_TIME_NS: AtomicU64 = AtomicU64::new(0);
static S2_TIME_NS: AtomicU64 = AtomicU64::new(0);
static S3_TIME_NS: AtomicU64 = AtomicU64::new(0);
static S4_TIME_NS: AtomicU64 = AtomicU64::new(0);
static S5_TIME_NS: AtomicU64 = AtomicU64::new(0);
static S6_TIME_NS: AtomicU64 = AtomicU64::new(0);

/// RAII timer that records elapsed nanoseconds to a static AtomicU64 on drop.
/// Used for per-strategy timing with zero per-call overhead beyond Instant::now().
struct StrategyTimer(&'static AtomicU64, std::time::Instant);
impl StrategyTimer {
    #[inline(always)]
    fn new(counter: &'static AtomicU64) -> Self {
        Self(counter, std::time::Instant::now())
    }
}
impl Drop for StrategyTimer {
    fn drop(&mut self) {
        self.0.fetch_add(self.1.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}
pub mod context;
pub mod frameworks;
pub mod import_resolver;
pub mod include_graph;
pub mod name_matcher;
pub mod path_alias;

pub use config::{
    PATH_ALIAS_CONFIG_FILES, PathAliasConfig, commit_config_hashes, detect_config_change,
};
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
    {
        let _timer = StrategyTimer::new(&S1_TIME_NS);
        if BuiltinFilter::is_builtin(&reference.name, ctx.file.language) {
            S1_COUNT.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    }

    // Strategy 2: Scope-local exact match
    {
        let _timer = StrategyTimer::new(&S2_TIME_NS);
        if let Some(scope_id) = reference.scope_id {
            if let Some(sym) = ctx.lookup_scoped(scope_id, &reference.name) {
                S2_COUNT.fetch_add(1, Ordering::Relaxed);
                return Some(ResolvedTarget {
                symbol_id: sym.id,
                confidence: Confidence::certain(),
                strategy: ResolutionStrategy::ExactMatch,
                provenance: Provenance::TreeSitter,
            });
        }
    }
    }

    // Strategy 3: Container/class-local
    {
        let _timer = StrategyTimer::new(&S3_TIME_NS);
        if let Some(source_sym) = reference.source_symbol {
            if let Some(source) = ctx.symbols_by_id.get(&source_sym) {
                if let Some(container) = source.container {
                    if let Some(container_sym) = ctx.symbols_by_id.get(&container) {
                        if let Some(scope) = container_sym.scope_id {
                            if let Some(sym) = ctx.lookup_scoped(scope, &reference.name) {
                                S3_COUNT.fetch_add(1, Ordering::Relaxed);
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
    }

    // Strategy 4: Same-file exact match
    {
        let _timer = StrategyTimer::new(&S4_TIME_NS);
        let same_file = ctx.find_in_file_by_name(&reference.name);
        if let Some(matched) =
            name_matcher.best_match(&same_file, &reference.name, Confidence::certain())
        {
            S4_COUNT.fetch_add(1, Ordering::Relaxed);
            return Some(ResolvedTarget {
                symbol_id: matched.symbol_id,
                confidence: matched.confidence,
                strategy: matched.strategy,
                provenance: matched.provenance,
            });
        }
    }

    // Strategy 5: Import/include resolution
    {
        let _timer = StrategyTimer::new(&S5_TIME_NS);
        if let Some(import_indices) = ctx.imports_by_name.get(&reference.name) {
            for &idx in import_indices {
                let import = &ctx.imports[idx];
                let import_local = import.local_name.as_deref().unwrap_or("");
                let matches_by_alias = !import_local.is_empty() && import_local == reference.name;

                if let Ok(candidates) = import_resolver.resolve_import(import) {
                    if let Ok(chain_candidates) =
                        import_resolver.resolve_through_reexports(import, candidates)
                    {
                        if matches_by_alias {
                            if let Some(first) = chain_candidates.first() {
                                S5_COUNT.fetch_add(1, Ordering::Relaxed);
                                return Some(ResolvedTarget {
                                    symbol_id: first.id,
                                    confidence: Confidence::new(0.8),
                                    strategy: ResolutionStrategy::ImportResolved,
                                    provenance: Provenance::Heuristic,
                                });
                            }
                        }
                        if let Some(matched) = name_matcher.best_match(
                            &chain_candidates,
                            &reference.name,
                            Confidence::certain(),
                        ) {
                            S5_COUNT.fetch_add(1, Ordering::Relaxed);
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
    }

    // Strategy 6: Project-wide name search + fuzzy fallback
    {
        let _timer = StrategyTimer::new(&S6_TIME_NS);
        if let Some(idx) = global_index {
            let candidates = match proximity_file_id {
                Some(fid) => idx.find_by_name_proximity(&reference.name, fid),
                None => idx.find_by_name(&reference.name),
            };
            if !candidates.is_empty() {
                if let Some(matched) =
                    name_matcher.best_match(&candidates, &reference.name, Confidence::new(0.6))
                {
                    S6_COUNT.fetch_add(1, Ordering::Relaxed);
                    S6_EXACT_COUNT.fetch_add(1, Ordering::Relaxed);
                    return Some(ResolvedTarget {
                        symbol_id: matched.symbol_id,
                        confidence: matched.confidence,
                        strategy: matched.strategy,
                        provenance: matched.provenance,
                    });
                }
            }
            if !should_run_fuzzy_fallback(&reference.name) {
                return None;
            }
            // Try fuzzy-with-proximity before full global scan.
            if let Some(fid) = proximity_file_id {
                let fuzzy_prox = idx.fuzzy_search_proximity(&reference.name, 2, fid);
                if !fuzzy_prox.is_empty() {
                    if let Some(matched) =
                        name_matcher.best_match(&fuzzy_prox, &reference.name, Confidence::new(0.4))
                    {
                        S6_COUNT.fetch_add(1, Ordering::Relaxed);
                        S6_FUZZY_PROX_COUNT.fetch_add(1, Ordering::Relaxed);
                        return Some(ResolvedTarget {
                            symbol_id: matched.symbol_id,
                            confidence: matched.confidence,
                            strategy: ResolutionStrategy::FuzzyMatch,
                            provenance: Provenance::Heuristic,
                        });
                    }
                }
            }
            let fuzzy = idx.fuzzy_search(&reference.name, 2);
            if !fuzzy.is_empty() {
                if let Some(matched) =
                    name_matcher.best_match(&fuzzy, &reference.name, Confidence::new(0.4))
                {
                    S6_COUNT.fetch_add(1, Ordering::Relaxed);
                    S6_FUZZY_GLOBAL_COUNT.fetch_add(1, Ordering::Relaxed);
                    return Some(ResolvedTarget {
                        symbol_id: matched.symbol_id,
                        confidence: matched.confidence,
                        strategy: ResolutionStrategy::FuzzyMatch,
                        provenance: Provenance::Heuristic,
                    });
                }
            }
        }
    }
    MISS_COUNT.fetch_add(1, Ordering::Relaxed);
    None
}

fn should_run_fuzzy_fallback(name: &str) -> bool {
    // Very short identifiers (`i`, `x`, `id`, `ok`) produce large ambiguous
    // candidate sets and weak evidence. Exact project-wide name lookup above
    // still handles them; this only disables edit-distance fallback.
    if name.chars().count() < 3 {
        return false;
    }
    // Skip names containing non-identifier characters (dots, slashes, etc.).
    // Symbol names are identifiers; a reference like `React.FC` can never
    // fuzzy-match any symbol.  S5 (import resolution) already handles qualified
    // paths; if it didn't resolve, fuzzy is guaranteed to miss.
    is_valid_identifier(name)
}

/// Returns true if `name` looks like a valid code identifier
/// (`[a-zA-Z_$][a-zA-Z0-9_$]*`). Characters outside this set cannot
/// match any symbol name via edit distance.
fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        None => return false,
        Some(c) if !c.is_ascii_alphabetic() && c != '_' && c != '$' => return false,
        _ => {}
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
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
            import_resolver: Arc::new(ImportResolver::with_path_alias(store.clone(), path_alias)),
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
        refs: &[(FileId, Vec<ReferenceUse>)], // Single-element batch for this file
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

    /// Lightweight resolution for clean files: only strategy 1 (builtin) +
    /// strategy 6 (global name search).  Strategies 2–5 are file-context-dependent
    /// and would produce the same results as last time (file hasn't changed).
    ///
    /// This is used by P3 per-file resolution fingerprints: clean files skip
    /// the expensive `ResolutionContext::build()` but their unresolved refs
    /// may now resolve against new symbols from dirty files.
    pub fn resolve_global_only(
        &self,
        reference: &ReferenceUse,
        file_language: Language,
    ) -> Option<ResolvedTarget> {
        // Strategy 1: Built-in / external filter
        if BuiltinFilter::is_builtin(&reference.name, file_language) {
            return None;
        }

        // Strategy 6: Project-wide name search + fuzzy fallback
        let candidates = self.global_index.find_by_name(&reference.name);
        if !candidates.is_empty() {
            if let Some(matched) =
                self.name_matcher
                    .best_match(&candidates, &reference.name, Confidence::new(0.6))
            {
                S6_COUNT.fetch_add(1, Ordering::Relaxed);
                S6_EXACT_COUNT.fetch_add(1, Ordering::Relaxed);
                return Some(ResolvedTarget {
                    symbol_id: matched.symbol_id,
                    confidence: matched.confidence,
                    strategy: matched.strategy,
                    provenance: matched.provenance,
                });
            }
        }
        if should_run_fuzzy_fallback(&reference.name) {
            let fuzzy = self.global_index.fuzzy_search(&reference.name, 2);
            if !fuzzy.is_empty() {
                if let Some(matched) =
                    self.name_matcher
                        .best_match(&fuzzy, &reference.name, Confidence::new(0.4))
                {
                    S6_COUNT.fetch_add(1, Ordering::Relaxed);
                    S6_FUZZY_GLOBAL_COUNT.fetch_add(1, Ordering::Relaxed);
                    return Some(ResolvedTarget {
                        symbol_id: matched.symbol_id,
                        confidence: matched.confidence,
                        strategy: ResolutionStrategy::FuzzyMatch,
                        provenance: Provenance::Heuristic,
                    });
                }
            }
        }
        MISS_COUNT.fetch_add(1, Ordering::Relaxed);
        None
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
        let unresolved = self.store.find_unresolved_references()?;
        let total_refs = unresolved.len();

        let mut by_file: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
        for r in &unresolved {
            by_file.entry(r.file_id).or_default().push(r.clone());
        }

        self.resolve_grouped_refs(by_file, total_refs)
    }

    /// Resolve a pre-grouped map of references (file_id → references).
    ///
    /// Shared by [`resolve_all`] and [`resolve_for_files`] — the only
    /// difference between the two is how references are collected.
    fn resolve_grouped_refs(
        &mut self,
        by_file: HashMap<FileId, Vec<ReferenceUse>>,
        total_refs: usize,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        if self.global_index.is_none() {
            self.global_index = Some(GlobalSymbolIndex::build(&self.store)?);
        }

        let mut stats = ResolutionStats {
            total_refs,
            ..Default::default()
        };

        let mut pending_resolutions: Vec<(ReferenceId, ResolvedTarget)> = Vec::new();
        let mut callsite_pairs: Vec<(ReferenceId, SymbolId)> = Vec::new();
        let mut all_resolved: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
        let batch_size = 500;

        for (file_id, refs) in &by_file {
            let ctx = match ResolutionContext::build(&self.store, *file_id) {
                Ok(c) => c,
                Err(e) => {
                    stats.add_warning(format!("failed to build context: {e}"));
                    continue;
                }
            };

            for reference in refs {
                match self.resolve_one(reference, &ctx) {
                    Some(target) => {
                        callsite_pairs.push((reference.id, target.symbol_id));
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
        if !callsite_pairs.is_empty() {
            self.store.update_callsite_callees_batch(&callsite_pairs)?;
        }
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
        _on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        if let Some(mutex) = progress_mutex {
            mutex
                .lock()
                .unwrap()
                .start_phase(ProgressPhase::Resolution, None);
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

        // P3: Per-file resolution fingerprint — split files into dirty (need
        // full resolution) and clean (fingerprint matches content_hash).
        let mut dirty_refs: Vec<(FileId, Vec<ReferenceUse>)> = Vec::with_capacity(by_file.len());
        // clean file refs: (FileId, file_language, Vec<ReferenceUse>)
        let mut clean_file_refs: Vec<(FileId, Language, Vec<ReferenceUse>)> =
            Vec::with_capacity(by_file.len() / 2);
        // Track dirty file (id, content_hash) for fingerprint update after resolution.
        let mut dirty_file_hashes: Vec<(FileId, String)> = Vec::with_capacity(by_file.len());
        for (fid, refs) in by_file {
            let file_info = store.get_file(&fid).ok().flatten();
            let fp = store.get_resolution_fingerprint(&fid).ok().flatten();
            let is_clean = match (&fp, &file_info) {
                (Some(fp), Some(info)) => *fp == info.content_hash,
                _ => false,
            };
            if is_clean {
                // file_info is guaranteed Some here (is_clean requires it)
                let info = file_info.unwrap();
                clean_file_refs.push((fid, info.language, refs));
            } else {
                dirty_refs.push((fid, refs));
                if let Some(info) = file_info {
                    dirty_file_hashes.push((fid, info.content_hash));
                }
            }
        }

        // ── Phase 1 + Phase 2: streaming via bounded mpsc channel ──
        //
        // Step A (serial, 1 lock): Pre-build ResolutionContext for dirty files only.
        // Clean files skip context build — their context hasn't changed.
        //
        // Step B (parallel, rayon): Resolve dirty files' refs against pre-built
        // contexts.  Then resolve clean files' refs using strategy-6-only
        // (global name search, no context needed).
        //
        // Phase 2 (writer thread): Accumulates batches of 2000 resolved refs,
        // writes them to the DB, and updates progress.  Also collects the full
        // all_resolved Vec for the caller (graph builder needs it).
        let matched_counter = Arc::new(AtomicU64::new(0));
        let session = &session;

        let progress_atomic = progress_mutex.map(|a| Arc::clone(&a.lock().expect("progress_mutex lock poisoned").atomic_current));

        // Step A: build contexts for dirty files only
        let step_a_span = tracing::info_span!(target: "atlas_resolve", "resolution.step_a",
            dirty_file_count = dirty_refs.len(),
            clean_file_count = clean_file_refs.len());
        let _step_a = step_a_span.enter();
        let mut file_groups: Vec<(FileId, Vec<ReferenceUse>, ResolutionContext)> =
            Vec::with_capacity(dirty_refs.len());
        for (fid, refs) in dirty_refs {
            match ResolutionContext::build(&store, fid) {
                Ok(ctx) => file_groups.push((fid, refs, ctx)),
                Err(_e) => { /* skip */ }
            }
        }
        drop(_step_a);

        // Bounded channel — capacity 4000 balances memory vs throughput.
        let (tx, rx) = mpsc::sync_channel::<(ReferenceUse, ResolvedTarget)>(4000);

        // Spawn Phase 2 writer thread that also collects all_resolved.
        let writer_store = store.clone();
        let writer_progress = progress_mutex.map(Arc::clone);
        let writer_handle = std::thread::spawn(
            move || -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
                let _phase2_span = tracing::info_span!(target: "atlas_resolve", "resolution.phase2").entered();
                let mut stats = ResolutionStats {
                    total_refs: total_refs as usize,
                    ..Default::default()
                };
                let mut pending: Vec<(ReferenceId, ResolvedTarget)> = Vec::with_capacity(2000);
                let mut callsite_pairs: Vec<(ReferenceId, SymbolId)> = Vec::new();
                let mut all: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
                let batch_size = 2000;
                let mut processed = 0u64;

                for (reference, target) in rx {
                    callsite_pairs.push((reference.id, target.symbol_id));
                    pending.push((reference.id, target.clone()));
                    let strategy = target.strategy.as_str().to_string();
                    all.push((reference, target));
                    processed += 1;
                    stats.resolved += 1;
                    *stats.by_strategy.entry(strategy).or_default() += 1;

                    if pending.len() >= batch_size {
                        writer_store.batch_update_resolutions(&pending)?;
                        pending.clear();
                        if let Some(ref ps) = writer_progress {
                            let _ = ps.lock().map(|mut p| p.set_current(processed));
                        }
                    }
                }
                if !pending.is_empty() {
                    writer_store.batch_update_resolutions(&pending)?;
                    if let Some(ref ps) = writer_progress {
                        let _ = ps.lock().map(|mut p| p.set_current(processed));
                    }
                }
                if !callsite_pairs.is_empty() {
                    writer_store.update_callsite_callees_batch(&callsite_pairs)?;
                }
                stats.unresolved = total_refs as usize - stats.resolved;
                Ok((all, stats))
            },
        );

        // Enter Phase 2 progress bar before spawning rayon to show percentage.
        if let Some(ps) = progress_mutex {
            let _ = ps.lock().map(|mut p| p.enter_phase2(total_refs));
        }

        // Step B: pure-memory parallel resolution — send results to channel.
        let step_b_span =
            tracing::info_span!(target: "atlas_resolve", "resolution.step_b", dirty_file_count = file_groups.len());
        let _step_b = step_b_span.enter();
        let mc = &matched_counter;

        // Resolve dirty files' refs with full context (6 strategies)
        file_groups.par_iter().for_each(|(_fid, refs, ctx)| {
            let results = session.resolve_refs_in_ctx(refs, ctx);
            let count = results.len() as u64;
            let total = mc.fetch_add(count, Ordering::Relaxed) + count;
            if let Some(ref ac) = progress_atomic {
                ac.store(total, Ordering::Relaxed);
            }
            for r in results {
                if tx.send(r).is_err() {
                    break;
                }
            }
        });

        // Resolve clean files' unresolved refs with strategy-6-only (global index).
        // No context needed — the file's symbols/imports/scopes haven't changed,
        // so strategies 2-5 would produce identical results.  New symbols from
        // dirty files may now resolve previously-unresolved references.
        if !clean_file_refs.is_empty() {
            let clean_step_span = tracing::info_span!(
                target: "atlas_resolve",
                "resolution.clean_files",
                count = clean_file_refs.len()
            );
            let _clean_step = clean_step_span.enter();
            let tx_ref = &tx;
            let mc_ref = mc;
            let progress_atomic_ref = progress_atomic.as_ref();

            for (_fid, lang, refs) in &clean_file_refs {
                let clean_results: Vec<_> = refs
                    .iter()
                    .filter_map(|r| {
                        session
                            .resolve_global_only(r, *lang)
                            .map(|t| (r.clone(), t))
                    })
                    .collect();
                let count = clean_results.len() as u64;
                let total = mc_ref.fetch_add(count, Ordering::Relaxed) + count;
                if let Some(ac) = progress_atomic_ref {
                    ac.store(total, Ordering::Relaxed);
                }
                for r in clean_results {
                    if tx_ref.send(r).is_err() {
                        break;
                    }
                }
            }
            drop(_clean_step);
        }

        drop(tx);
        drop(_step_b);

        match writer_handle.join() {
            Ok(Ok((all_resolved, stats))) => {
                // P3: Update resolution fingerprints for dirty files.
                // After resolution completes, store the content_hash as the
                // fingerprint so the next run can skip this file if unchanged.
                for (fid, content_hash) in &dirty_file_hashes {
                    let _ = store.update_resolution_fingerprint(fid, content_hash);
                }

                let s1 = S1_COUNT.load(Ordering::Relaxed);
                let s2 = S2_COUNT.load(Ordering::Relaxed);
                let s3 = S3_COUNT.load(Ordering::Relaxed);
                let s4 = S4_COUNT.load(Ordering::Relaxed);
                let s5 = S5_COUNT.load(Ordering::Relaxed);
                let s6 = S6_COUNT.load(Ordering::Relaxed);
                let miss = MISS_COUNT.load(Ordering::Relaxed);
                let total = s1 + s2 + s3 + s4 + s5 + s6 + miss;
                if total > 0 {
                    let total_f = total as f64;
                    tracing::info!(
                        target: "atlas_resolve",
                        s1 = s1, s1_pct = (100.0 * s1 as f64 / total_f),
                        s2 = s2, s2_pct = (100.0 * s2 as f64 / total_f),
                        s3 = s3, s3_pct = (100.0 * s3 as f64 / total_f),
                        s4 = s4, s4_pct = (100.0 * s4 as f64 / total_f),
                        s5 = s5, s5_pct = (100.0 * s5 as f64 / total_f),
                        s6 = s6, s6_pct = (100.0 * s6 as f64 / total_f),
                        miss = miss, miss_pct = (100.0 * miss as f64 / total_f),
                        total = total,
                        "resolution.summary"
                    );
                    let s6_exact = S6_EXACT_COUNT.load(Ordering::Relaxed);
                    let s6_fuzzy_prox = S6_FUZZY_PROX_COUNT.load(Ordering::Relaxed);
                    let s6_fuzzy_global = S6_FUZZY_GLOBAL_COUNT.load(Ordering::Relaxed);
                    if s6 > 0 {
                        let s6_f = s6 as f64;
                        tracing::info!(
                            target: "atlas_resolve",
                            s6_exact = s6_exact, s6_exact_pct = (100.0 * s6_exact as f64 / s6_f),
                            s6_fuzzy_prox = s6_fuzzy_prox, s6_fuzzy_prox_pct = (100.0 * s6_fuzzy_prox as f64 / s6_f),
                            s6_fuzzy_global = s6_fuzzy_global, s6_fuzzy_global_pct = (100.0 * s6_fuzzy_global as f64 / s6_f),
                            "resolution.s6_breakdown"
                        );
                    }
                    tracing::info!(
                        target: "atlas_resolve",
                        s1_s = S1_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9,
                        s2_s = S2_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9,
                        s3_s = S3_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9,
                        s4_s = S4_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9,
                        s5_s = S5_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9,
                        s6_s = S6_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9,
                        "resolution.strategy_times"
                    );
                }
                Ok((all_resolved, stats))
            },
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                Err(anyhow::anyhow!("Phase 2 writer panicked: {msg}"))
            }
        }
    }

    /// Wrapper that builds the session internally (convenience when you
    /// already have a `ReferenceResolver` and just want parallelism).
    pub fn resolve_all_parallel_simple(
        &mut self,
        progress_mutex: Option<&std::sync::Arc<std::sync::Mutex<types::progress::ProgressState>>>,
        _on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        let store = self.store.clone();
        self.resolve_all_parallel(store, progress_mutex, _on_progress)
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
        let mut by_file: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
        for fid in file_ids {
            for r in self.store.find_references_by_file(fid)? {
                by_file.entry(r.file_id).or_default().push(r);
            }
        }

        let total_refs: usize = by_file.values().map(|v| v.len()).sum();
        self.resolve_grouped_refs(by_file, total_refs)
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
            stats.add_warning(format!("batch resolution update failed: {e}"));
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

    #[test]
    fn short_names_skip_edit_distance_fuzzy_fallback() {
        assert!(!should_run_fuzzy_fallback("i"));
        assert!(!should_run_fuzzy_fallback("id"));
        assert!(should_run_fuzzy_fallback("idx"));
    }

    #[test]
    fn non_identifier_names_skip_fuzzy() {
        // Dot-path references (e.g. React.FC) never match any symbol
        assert!(!should_run_fuzzy_fallback("React.FC"));
        assert!(!should_run_fuzzy_fallback("styled.div"));
        // Special chars
        assert!(!should_run_fuzzy_fallback("{createElement}"));
    }

    #[test]
    fn valid_identifier_names_allow_fuzzy() {
        assert!(should_run_fuzzy_fallback("logger"));
        assert!(should_run_fuzzy_fallback("_private"));
        assert!(should_run_fuzzy_fallback("$event"));
        assert!(!should_run_fuzzy_fallback("7zip")); // starts with digit
    }

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
            "expected main to be caller of greet, got: {caller_names:?}"
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
            "expected greet to be callee of main, got: {callee_names:?}"
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
        store
            .insert_file_facts(&main_facts)
            .expect("insert main.ts");

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
            "expected greet to be callee of main (via aliased import 'hello'), got: {callee_names:?}"
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
            "expected main to be caller of greet (via aliased import 'hello'), got: {caller_names:?}"
        );
    }

    /// Smoke test: verify that all new tracing spans do not panic when
    /// a subscriber is active.  Exercises resolve_one_core (6 strategies),
    /// GlobalSymbolIndex::fuzzy_search, ResolutionContext::find_in_file_by_name,
    /// and the resolve_all pipeline (step A/B spans, phase2 span).
    #[test]
    fn tracing_spans_do_not_panic() {
        let subscriber = tracing_subscriber::fmt()
            .with_test_writer()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let store = Arc::new(Store::open_in_memory().unwrap());
            store.init_schema().unwrap();

            let lib_src = r#"export function greet(name: string): string { return 'Hello, ' + name; }"#;
            let main_src = r#"import { greet } from './lib';
function main() {
    greet("World");
}
main();
"#;
            let ts = create_frontend(Language::TypeScript).unwrap();

            let lib_id = FileId::generate("lib.ts");
            let lib = extract_file(
                &ts,
                lib_id,
                &PathBuf::from("lib.ts"),
                lib_src,
                "abc",
            )
            .unwrap();
            store.insert_file_facts(&lib).unwrap();

            let main_id = FileId::generate("main.ts");
            let main = extract_file(
                &ts,
                main_id,
                &PathBuf::from("main.ts"),
                main_src,
                "abc",
            )
            .unwrap();
            store.insert_file_facts(&main).unwrap();

            let mut resolver = ReferenceResolver::new(Arc::clone(&store));
            let (_resolved, stats) = resolver.resolve_all().unwrap();
            assert!(
                stats.resolved > 0,
                "expected at least 1 resolved reference, got {}",
                stats.resolved
            );
            // Also exercise fuzzy_search directly to cover the cache path
            let index = GlobalSymbolIndex::build(&store).unwrap();
            let _ = index.fuzzy_search("greeet", 2);
            let _ = index.fuzzy_search("greeet", 2); // cache hit path
            // Exercise find_in_file_by_name span
            let ctx = ResolutionContext::build(&store, lib_id).unwrap();
            let _ = ctx.find_in_file_by_name("greet");
        });
    }
}
