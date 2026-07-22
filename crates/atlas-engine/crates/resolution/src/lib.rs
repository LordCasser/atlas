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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use db::Store;
use rayon::prelude::*;
use types::progress::ProgressPhase;
use types::*;

use self::builtins::BuiltinFilter;
use self::context::{GlobalSymbolIndex, ResolutionContext, is_explicit_test_path, proximity_tier};
use self::import_resolver::ImportResolver;
use self::name_matcher::NameMatcher;

type ResolvedReference = (ReferenceUse, ResolvedTarget);
type WriterOutput = (Vec<ResolvedReference>, ResolutionStats, WriterTelemetry);
pub type VisibilityFilterFn = dyn Fn(&SymbolDef, FileId) -> bool;
type StagedResolutionRow = (
    String,
    i64,
    Vec<u8>,
    String,
    Option<Vec<u8>>,
    String,
    String,
    String,
    Option<String>,
);

struct ScopedResolutionState<'a> {
    context: &'a ResolutionContext,
    visibility_filter: Option<&'a VisibilityFilterFn>,
    preferred_files: HashSet<FileId>,
    source_is_test: bool,
    source_parent: Option<String>,
    candidate_cache: &'a mut HashMap<String, Vec<SymbolDef>>,
    file_path_cache: &'a mut HashMap<FileId, String>,
}

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
        self.0
            .fetch_add(self.1.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}
pub mod context;
pub mod import_resolver;
pub mod name_matcher;
pub mod path_alias;

pub use config::{
    PATH_ALIAS_CONFIG_FILES, PathAliasConfig, commit_config_hashes, detect_config_change,
};
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
    file_scope_cache: &std::sync::Mutex<HashMap<FileId, HashSet<FileId>>>,
) -> Option<ResolvedTarget> {
    if is_builtin_reference(reference, ctx.file.language) {
        return None;
    }

    // Contextual strategies 2-5: shared implementation.
    if let Some(result) =
        resolve_contextual_strategies(reference, ctx, import_resolver, name_matcher)
    {
        return Some(result);
    }

    // An explicit import binding is a semantic boundary. If its target module
    // does not export the requested name, a project-wide same-name symbol is
    // not a valid substitute. Module-only/wildcard imports are not indexed by
    // name and therefore retain the normal Strategy 6 behavior.
    if ctx.imports_by_name.contains_key(&reference.name) {
        MISS_COUNT.fetch_add(1, Ordering::Relaxed);
        return None;
    }

    // Strategy 6: Project-wide name search with import-scoped pre-filtering
    {
        let _timer = StrategyTimer::new(&S6_TIME_NS);
        if let Some(idx) = global_index {
            // Compute per-file preferred scope (reuses ImportResolver infra)
            let preferred = {
                let mut cache = file_scope_cache.lock().unwrap();
                cache
                    .entry(reference.file_id)
                    .or_insert_with(|| import_resolver.collect_imported_file_ids(&ctx.imports))
                    .clone()
            };

            let exact = if !preferred.is_empty() {
                idx.find_exact_name_target_in_scope(&reference.name, proximity_file_id, &preferred)
            } else {
                idx.find_exact_name_target(&reference.name, proximity_file_id)
            };
            if let Some(matched) = exact {
                S6_COUNT.fetch_add(1, Ordering::Relaxed);
                S6_EXACT_COUNT.fetch_add(1, Ordering::Relaxed);
                return Some(matched);
            }
            if !should_run_fuzzy_fallback_for_reference(reference) {
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

fn is_builtin_reference(reference: &ReferenceUse, language: Language) -> bool {
    let _timer = StrategyTimer::new(&S1_TIME_NS);
    let builtin = (language == Language::ArkTS && reference.kind == ReferenceKind::Decoration)
        || BuiltinFilter::is_builtin(&reference.name, language);
    if builtin {
        S1_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    builtin
}

/// Run contextual Strategies 2–5: scope-local, container-local, same-file
/// exact, and import resolution. Builtins are terminal and must be rejected by
/// the caller before this function. Returns `None` to fall through to Strategy 6.
pub(crate) fn resolve_contextual_strategies(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    import_resolver: &ImportResolver,
    name_matcher: &NameMatcher,
) -> Option<ResolvedTarget> {
    // Strategy 2: Scope-local exact match
    {
        let _timer = StrategyTimer::new(&S2_TIME_NS);
        if let Some(scope_id) = reference.scope_id {
            if let Some(sym) = ctx
                .lookup_scoped(scope_id, &reference.name)
                .filter(|sym| scoped_kind_is_compatible(reference.kind, sym.kind))
            {
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
                            if let Some(sym) = ctx
                                .lookup_scoped(scope, &reference.name)
                                .filter(|sym| scoped_kind_is_compatible(reference.kind, sym.kind))
                            {
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
        let same_file = ctx
            .find_in_file_by_name(&reference.name)
            .into_iter()
            .filter(|symbol| scoped_kind_is_compatible(reference.kind, symbol.kind))
            .collect::<Vec<_>>();
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
                        import_resolver.resolve_through_reexports(import, candidates.clone())
                    {
                        let chain_remapped = chain_candidates
                            .iter()
                            .any(|candidate| !candidates.iter().any(|c| c.id == candidate.id));
                        let compatible = chain_candidates
                            .iter()
                            .filter(|symbol| scoped_kind_is_compatible(reference.kind, symbol.kind))
                            .cloned()
                            .collect::<Vec<_>>();
                        if matches_by_alias || chain_remapped {
                            if let Some(first) = compatible.first() {
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
                            &compatible,
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

fn should_run_fuzzy_fallback_for_reference(reference: &ReferenceUse) -> bool {
    if reference.kind == ReferenceKind::Call {
        return false;
    }
    should_run_fuzzy_fallback(&reference.name)
}

fn scoped_kind_is_compatible(reference_kind: ReferenceKind, symbol_kind: SymbolKind) -> bool {
    if reference_kind != ReferenceKind::Call {
        return true;
    }
    matches!(
        symbol_kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Constructor
            | SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Interface
            | SymbolKind::Trait
    )
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

/// Map a resolved target's strategy to resolution scope and coverage tier.
///
/// Strategies 1-5 (local scope) → closure_complete
/// Strategy 6 (project-wide name search) → boundary
fn scope_and_tier(target: &ResolvedTarget) -> (&'static str, &'static str) {
    match target.strategy {
        ResolutionStrategy::ExactMatch | ResolutionStrategy::ImportResolved => {
            ("closure_complete", "closure_complete")
        }
        ResolutionStrategy::NameOnly
        | ResolutionStrategy::FuzzyMatch
        | ResolutionStrategy::Heuristic => ("boundary", "boundary"),
        ResolutionStrategy::Builtin | ResolutionStrategy::DataflowPointer => {
            ("boundary", "boundary")
        }
    }
}

/// Map a confidence value to a [`SemanticConfidence`] string.
fn confidence_to_semantic(confidence: f64) -> String {
    if confidence >= 0.95 {
        "certain".to_string()
    } else if confidence >= 0.75 {
        "high".to_string()
    } else if confidence >= 0.5 {
        "medium".to_string()
    } else {
        "low".to_string()
    }
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

    /// Build the session with a pre-loaded symbol slice.
    ///
    /// Avoids a duplicate `get_all_symbols()` call when the caller already
    /// holds the symbol list. The store is still needed for ImportResolver
    /// and the file → parent directory map in GlobalSymbolIndex.
    pub fn build_from_symbols(store: Arc<Store>, symbols: &[SymbolDef]) -> anyhow::Result<Self> {
        Ok(Self {
            global_index: Arc::new(GlobalSymbolIndex::build_from_symbols(symbols, &store)?),
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

    /// Build with path alias support and pre-loaded symbols.
    pub fn build_with_alias_from_symbols(
        store: Arc<Store>,
        path_alias: PathAliasResolver,
        symbols: &[SymbolDef],
    ) -> anyhow::Result<Self> {
        Ok(Self {
            global_index: Arc::new(GlobalSymbolIndex::build_from_symbols(symbols, &store)?),
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
        let file_scope_cache = std::sync::Mutex::new(HashMap::<FileId, HashSet<FileId>>::new());
        for (file_id, references) in refs {
            let ctx = ResolutionContext::build(store, *file_id)?;
            for reference in references {
                if let Some(target) = self.resolve_one(reference, &ctx, &file_scope_cache) {
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
        file_scope_cache: &std::sync::Mutex<HashMap<FileId, HashSet<FileId>>>,
    ) -> Vec<(ReferenceUse, ResolvedTarget)> {
        let mut results = Vec::with_capacity(references.len());
        for reference in references {
            if let Some(target) = self.resolve_one(reference, ctx, file_scope_cache) {
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
        file_scope_cache: &std::sync::Mutex<HashMap<FileId, HashSet<FileId>>>,
    ) -> Option<ResolvedTarget> {
        resolve_one_core(
            reference,
            ctx,
            &self.import_resolver,
            &self.name_matcher,
            Some(&self.global_index),
            Some(ctx.file.file_id),
            file_scope_cache,
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
        if is_builtin_reference(reference, file_language) {
            return None;
        }

        // Strategy 6: Project-wide name search + fuzzy fallback
        if let Some(matched) = self
            .global_index
            .find_exact_name_target(&reference.name, None)
        {
            S6_COUNT.fetch_add(1, Ordering::Relaxed);
            S6_EXACT_COUNT.fetch_add(1, Ordering::Relaxed);
            return Some(matched);
        }
        if should_run_fuzzy_fallback_for_reference(reference) {
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
/// Project resolution only resolves references and updates the `"references"`
/// table. Edge creation is delegated to `GraphBuilder`.
///
/// P4: Uses `GlobalSymbolIndex` for project-wide name search instead of
/// per-reference FTS5 queries. The global index is built once at the start
/// of resolution.
///
/// `resolve_all_parallel()` uses rayon to parallelize per-file resolution,
/// with a Phase-1 (parallel matching) → Phase-2 (serial write) model.
pub struct ReferenceResolver {
    store: Arc<Store>,
    import_resolver: ImportResolver,
    name_matcher: NameMatcher,
    /// Global in-memory symbol index used by scoped file resolution.
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

    /// Resolve a pre-grouped map of references (file_id → references).
    ///
    /// Used by scoped file resolution after the caller has selected the exact
    /// files whose references should be resolved.
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
        let mut all_resolved: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
        let batch_size = 500;

        let file_scope_cache = std::sync::Mutex::new(HashMap::<FileId, HashSet<FileId>>::new());

        for (file_id, refs) in &by_file {
            let ctx = match ResolutionContext::build(&self.store, *file_id) {
                Ok(c) => c,
                Err(e) => {
                    stats.add_warning(format!("failed to build context: {e}"));
                    continue;
                }
            };

            for reference in refs {
                match self.resolve_one(reference, &ctx, &file_scope_cache) {
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
        _on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        self.resolve_all_parallel_impl(store, None, progress_mutex, _on_progress)
    }

    /// Like [`resolve_all_parallel`], but uses pre-loaded symbols for the
    /// global index instead of calling `store.get_all_symbols()` internally.
    ///
    /// This avoids a duplicate DB query when the caller already holds the
    /// symbol list (e.g. for shared use with `GraphBuilder::symbol_override`).
    pub fn resolve_all_parallel_with_symbols(
        &mut self,
        store: Arc<Store>,
        symbols: &[SymbolDef],
        progress_mutex: Option<&std::sync::Arc<std::sync::Mutex<types::progress::ProgressState>>>,
        _on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        self.resolve_all_parallel_impl(store, Some(symbols), progress_mutex, _on_progress)
    }

    /// Shared implementation of parallel resolution.
    ///
    /// `symbols` is an optional pre-loaded slice of project symbols.
    /// When `Some`, the global index is built from the slice instead of
    /// calling `store.get_all_symbols()`. When `None`, the session loads
    /// symbols from the store as usual.
    fn resolve_all_parallel_impl(
        &mut self,
        store: Arc<Store>,
        symbols: Option<&[SymbolDef]>,
        progress_mutex: Option<&std::sync::Arc<std::sync::Mutex<types::progress::ProgressState>>>,
        _on_progress: Option<&dyn Fn(u64, u64)>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        let mut telemetry = ResolutionTelemetry::default();

        if let Some(mutex) = progress_mutex {
            mutex
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .start_phase(ProgressPhase::Resolution, None);
        }

        // Build shared session
        let t0 = Instant::now();
        let session = match symbols {
            Some(syms) => ResolutionSession::build_from_symbols(store.clone(), syms)?,
            None => ResolutionSession::build(store.clone())?,
        };
        telemetry.session_build_ms = t0.elapsed().as_millis() as u64;

        // Load all unresolved references, grouped by file
        let t_load = Instant::now();
        let unresolved = store.find_unresolved_references()?;
        telemetry.unresolved_load_ms = t_load.elapsed().as_millis() as u64;
        telemetry.unresolved_refs = unresolved.len();
        let total_refs = unresolved.len() as u64;
        let t_group = Instant::now();
        let by_file: Vec<(FileId, Vec<ReferenceUse>)> = {
            let mut map: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
            for r in &unresolved {
                map.entry(r.file_id).or_default().push(r.clone());
            }
            map.into_iter().collect()
        };
        telemetry.group_by_file_ms = t_group.elapsed().as_millis() as u64;
        telemetry.files_with_refs = by_file.len();

        // P3: Per-file resolution fingerprint — split files into dirty (need
        // full resolution) and clean (fingerprint matches content_hash).
        let t_split = Instant::now();
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
        telemetry.dirty_clean_split_ms = t_split.elapsed().as_millis() as u64;
        telemetry.dirty_files = dirty_refs.len();
        telemetry.clean_files = clean_file_refs.len();
        telemetry.dirty_refs = dirty_refs.iter().map(|(_, r)| r.len()).sum();
        telemetry.clean_refs = clean_file_refs.iter().map(|(_, _, r)| r.len()).sum();

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
        let scanned_counter = Arc::new(AtomicU64::new(0));
        let writer_processed_counter = Arc::new(AtomicU64::new(0));
        let writer_batch_counter = Arc::new(AtomicU64::new(0));
        let dirty_files_done_counter = Arc::new(AtomicU64::new(0));
        let clean_files_done_counter = Arc::new(AtomicU64::new(0));
        let session = &session;

        let progress_atomic = progress_mutex
            .map(|a| Arc::clone(&a.lock().unwrap_or_else(|e| e.into_inner()).atomic_current));

        // Step A: build contexts for dirty files only
        let step_a_span = tracing::info_span!(target: "atlas_resolve", "resolution.step_a",
            dirty_file_count = dirty_refs.len(),
            clean_file_count = clean_file_refs.len());
        let _step_a = step_a_span.enter();
        let t_ctx = Instant::now();
        let cap = dirty_refs.len();
        let mut file_groups: Vec<(FileId, Vec<ReferenceUse>, ResolutionContext)> =
            Vec::with_capacity(cap);
        let mut context_build_timings: Vec<ContextBuildTiming> = Vec::with_capacity(cap);
        let mut context_build_failures: usize = 0;
        for (fid, refs) in dirty_refs {
            let t_file = Instant::now();
            match ResolutionContext::build(&store, fid) {
                Ok(ctx) => {
                    let elapsed = t_file.elapsed().as_micros() as u64;
                    context_build_timings.push(ContextBuildTiming {
                        file_id: fid,
                        refs_count: refs.len(),
                        symbols_count: ctx.symbols_by_id.len(),
                        scopes_count: ctx.scopes_by_id.len(),
                        imports_count: ctx.imports.len(),
                        elapsed_us: elapsed,
                    });
                    file_groups.push((fid, refs, ctx));
                }
                Err(_e) => {
                    context_build_failures += 1;
                }
            }
        }
        telemetry.context_build_ms = t_ctx.elapsed().as_millis() as u64;
        telemetry.context_build_timings = context_build_timings;
        telemetry.context_build_failures = context_build_failures;
        drop(_step_a);

        // Bounded channel — capacity 4000 balances memory vs throughput.
        let (tx, rx) = mpsc::sync_channel::<(ReferenceUse, ResolvedTarget)>(4000);

        // Spawn Phase 2 writer thread that also collects all_resolved.
        let writer_store = store.clone();
        let writer_matched = Arc::clone(&matched_counter);
        let writer_processed_progress = Arc::clone(&writer_processed_counter);
        let writer_batch_progress = Arc::clone(&writer_batch_counter);
        let writer_handle = std::thread::spawn(move || -> anyhow::Result<WriterOutput> {
            let _phase2_span =
                tracing::info_span!(target: "atlas_resolve", "resolution.phase2").entered();
            let writer_start = Instant::now();
            let sent_counter = writer_matched;
            let mut batch_id: u64 = 0;
            let mut slow_batches: u64 = 0;
            let mut stats = ResolutionStats {
                total_refs: total_refs as usize,
                ..Default::default()
            };
            let mut pending: Vec<(ReferenceId, ResolvedTarget)> = Vec::with_capacity(2000);
            let mut all: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
            let batch_size = 2000;
            let mut processed = 0u64;

            for (reference, target) in rx {
                pending.push((reference.id, target.clone()));
                let strategy = target.strategy.as_str().to_string();
                all.push((reference, target));
                processed += 1;
                stats.resolved += 1;
                *stats.by_strategy.entry(strategy).or_default() += 1;

                if pending.len() >= batch_size {
                    let batch_start = Instant::now();
                    writer_store.batch_update_resolutions(&pending)?;
                    let batch_elapsed = batch_start.elapsed();
                    let elapsed_ms = batch_elapsed.as_millis() as u64;
                    let rows_per_sec = if elapsed_ms > 0 {
                        (pending.len() as f64) / (elapsed_ms as f64) * 1000.0
                    } else {
                        0.0
                    };
                    if elapsed_ms > 2000 {
                        let lag = sent_counter
                            .load(Ordering::Relaxed)
                            .saturating_sub(processed);
                        tracing::warn!(
                            target: "atlas_db_write",
                            batch_id = batch_id,
                            rows = pending.len(),
                            elapsed_ms = elapsed_ms,
                            rows_per_sec = rows_per_sec,
                            pending_queue_lag = lag,
                            "writer.slow_batch"
                        );
                        slow_batches += 1;
                    }
                    batch_id += 1;
                    writer_batch_progress.store(batch_id, Ordering::Relaxed);
                    writer_processed_progress.store(processed, Ordering::Relaxed);
                    pending.clear();
                }
            }
            if !pending.is_empty() {
                let batch_start = Instant::now();
                writer_store.batch_update_resolutions(&pending)?;
                let batch_elapsed = batch_start.elapsed();
                let elapsed_ms = batch_elapsed.as_millis() as u64;
                let rows_per_sec = if elapsed_ms > 0 {
                    (pending.len() as f64) / (elapsed_ms as f64) * 1000.0
                } else {
                    0.0
                };
                if elapsed_ms > 2000 {
                    let lag = sent_counter
                        .load(Ordering::Relaxed)
                        .saturating_sub(processed);
                    tracing::warn!(
                        target: "atlas_db_write",
                        batch_id = batch_id,
                        rows = pending.len(),
                        elapsed_ms = elapsed_ms,
                        rows_per_sec = rows_per_sec,
                        pending_queue_lag = lag,
                        "writer.slow_batch"
                    );
                    slow_batches += 1;
                }
                batch_id += 1;
                writer_batch_progress.store(batch_id, Ordering::Relaxed);
                writer_processed_progress.store(processed, Ordering::Relaxed);
            }
            stats.unresolved = total_refs as usize - stats.resolved;
            let writer_elapsed = writer_start.elapsed();
            let writer_total_ms = writer_elapsed.as_millis() as u64;
            let rows_per_sec = if writer_total_ms > 0 {
                processed as f64 / writer_total_ms as f64 * 1000.0
            } else {
                0.0
            };
            let writer_tel = WriterTelemetry {
                total_ms: writer_total_ms,
                batches: batch_id,
                rows_written: processed,
                rows_per_sec,
                slow_batch_count: slow_batches,
            };
            Ok((all, stats, writer_tel))
        });

        // Enter Phase 2 progress bar before spawning rayon to show percentage.
        if let Some(ps) = progress_mutex {
            let _ = ps.lock().map(|mut p| p.enter_phase2(total_refs));
        }

        let monitor_scanned = Arc::clone(&scanned_counter);
        let monitor_matched = Arc::clone(&matched_counter);
        let monitor_writer_processed = Arc::clone(&writer_processed_counter);
        let monitor_writer_batches = Arc::clone(&writer_batch_counter);
        let monitor_dirty_done = Arc::clone(&dirty_files_done_counter);
        let monitor_clean_done = Arc::clone(&clean_files_done_counter);
        let monitor_total_refs = total_refs;
        let monitor_dirty_files = file_groups.len() as u64;
        let monitor_clean_files = clean_file_refs.len() as u64;
        let (monitor_stop_tx, monitor_stop_rx) = mpsc::channel::<()>();
        let monitor_handle = std::thread::spawn(move || {
            let started = Instant::now();
            let mut last_tick = Instant::now();
            let mut last_scanned = 0u64;
            let mut last_written = 0u64;

            while monitor_stop_rx
                .recv_timeout(Duration::from_secs(10))
                .is_err()
            {
                let now = Instant::now();
                let interval_s = now.duration_since(last_tick).as_secs_f64();
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let scanned = monitor_scanned.load(Ordering::Relaxed);
                let matched = monitor_matched.load(Ordering::Relaxed);
                let written = monitor_writer_processed.load(Ordering::Relaxed);
                let batches = monitor_writer_batches.load(Ordering::Relaxed);
                let dirty_done = monitor_dirty_done.load(Ordering::Relaxed);
                let clean_done = monitor_clean_done.load(Ordering::Relaxed);
                let scanned_delta = scanned.saturating_sub(last_scanned);
                let written_delta = written.saturating_sub(last_written);
                let scan_refs_per_sec = if interval_s > 0.0 {
                    scanned_delta as f64 / interval_s
                } else {
                    0.0
                };
                let write_refs_per_sec = if interval_s > 0.0 {
                    written_delta as f64 / interval_s
                } else {
                    0.0
                };
                let queued_resolved = matched.saturating_sub(written);
                let match_rate_pct = if scanned > 0 {
                    100.0 * matched as f64 / scanned as f64
                } else {
                    0.0
                };
                let s5_count = S5_COUNT.load(Ordering::Relaxed);
                let s6_count = S6_COUNT.load(Ordering::Relaxed);
                let s6_exact = S6_EXACT_COUNT.load(Ordering::Relaxed);
                let s6_fuzzy_prox = S6_FUZZY_PROX_COUNT.load(Ordering::Relaxed);
                let s6_fuzzy_global = S6_FUZZY_GLOBAL_COUNT.load(Ordering::Relaxed);
                let miss_count = MISS_COUNT.load(Ordering::Relaxed);
                let s5_time_s = S5_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9;
                let s6_time_s = S6_TIME_NS.load(Ordering::Relaxed) as f64 / 1e9;

                tracing::info!(
                    target: "atlas_resolve",
                    elapsed_ms = elapsed_ms,
                    scanned_refs = scanned,
                    total_refs = monitor_total_refs,
                    matched_refs = matched,
                    match_rate_pct = match_rate_pct,
                    writer_rows_written = written,
                    writer_batches = batches,
                    queued_resolved = queued_resolved,
                    dirty_files_done = dirty_done,
                    dirty_files_total = monitor_dirty_files,
                    clean_files_done = clean_done,
                    clean_files_total = monitor_clean_files,
                    scan_refs_per_sec = scan_refs_per_sec,
                    write_refs_per_sec = write_refs_per_sec,
                    s5_count = s5_count,
                    s5_time_s = s5_time_s,
                    s6_count = s6_count,
                    s6_exact = s6_exact,
                    s6_fuzzy_prox = s6_fuzzy_prox,
                    s6_fuzzy_global = s6_fuzzy_global,
                    s6_time_s = s6_time_s,
                    miss_count = miss_count,
                    "resolution.progress"
                );

                last_tick = now;
                last_scanned = scanned;
                last_written = written;
            }
        });

        // Per-file cache: avoid recomputing imported-file scope for every S6
        // reference in the same file.
        let file_scope_cache: std::sync::Mutex<HashMap<FileId, HashSet<FileId>>> =
            std::sync::Mutex::new(HashMap::new());

        // Step B: pure-memory parallel resolution — send results to channel.
        let step_b_span = tracing::info_span!(
            target: "atlas_resolve",
            "resolution.step_b",
            dirty_file_count = file_groups.len()
        );
        let _step_b = step_b_span.enter();
        let mc = &matched_counter;
        let scanned = &scanned_counter;
        let dirty_done = &dirty_files_done_counter;
        let fsc = &file_scope_cache;

        // Resolve dirty files' refs with full context (6 strategies)
        let t_dirty = Instant::now();
        file_groups.par_iter().for_each(|(_fid, refs, ctx)| {
            let results = session.resolve_refs_in_ctx(refs, ctx, fsc);
            let count = results.len() as u64;
            mc.fetch_add(count, Ordering::Relaxed);
            let scanned_total =
                scanned.fetch_add(refs.len() as u64, Ordering::Relaxed) + refs.len() as u64;
            if let Some(ref ac) = progress_atomic {
                ac.store(scanned_total, Ordering::Relaxed);
            }
            for r in results {
                if tx.send(r).is_err() {
                    break;
                }
            }
            dirty_done.fetch_add(1, Ordering::Relaxed);
        });
        telemetry.resolve_dirty_ms = t_dirty.elapsed().as_millis() as u64;

        // Resolve clean files' unresolved refs with strategy-6-only (global index).
        // No context needed — the file's symbols/imports/scopes haven't changed,
        // so strategies 2-5 would produce identical results.  New symbols from
        // dirty files may now resolve previously-unresolved references.
        if !clean_file_refs.is_empty() {
            let t_clean = Instant::now();
            let clean_step_span = tracing::info_span!(
                target: "atlas_resolve",
                "resolution.clean_files",
                count = clean_file_refs.len()
            );
            let _clean_step = clean_step_span.enter();
            let tx_ref = &tx;
            let mc_ref = mc;
            let scanned_ref = scanned;
            let clean_done_ref = &clean_files_done_counter;
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
                mc_ref.fetch_add(count, Ordering::Relaxed);
                let scanned_total =
                    scanned_ref.fetch_add(refs.len() as u64, Ordering::Relaxed) + refs.len() as u64;
                if let Some(ac) = progress_atomic_ref {
                    ac.store(scanned_total, Ordering::Relaxed);
                }
                for r in clean_results {
                    if tx_ref.send(r).is_err() {
                        break;
                    }
                }
                clean_done_ref.fetch_add(1, Ordering::Relaxed);
            }
            drop(_clean_step);
            telemetry.resolve_clean_ms = t_clean.elapsed().as_millis() as u64;
        }

        drop(tx);
        drop(_step_b);

        let writer_result = writer_handle.join();
        let _ = monitor_stop_tx.send(());
        let _ = monitor_handle.join();

        match writer_result {
            Ok(Ok((all_resolved, stats, w))) => {
                telemetry.writer_total_ms = w.total_ms;
                telemetry.writer_batches = w.batches;
                telemetry.writer_rows_written = w.rows_written;
                telemetry.writer_rows_per_sec = w.rows_per_sec;
                // P3: Update resolution fingerprints for dirty files.
                // After resolution completes, store the content_hash as the
                // fingerprint so the next run can skip this file if unchanged.
                let t_fp = Instant::now();
                for (fid, content_hash) in &dirty_file_hashes {
                    let _ = store.update_resolution_fingerprint(fid, content_hash);
                }
                telemetry.fingerprint_update_ms = t_fp.elapsed().as_millis() as u64;

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
                log_resolution_telemetry(&telemetry);
                log_context_build_distribution(&telemetry.context_build_timings);
                Ok((all_resolved, stats))
            }
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

    /// Resolve references scoped to a closure, writing results to the
    /// `reference_resolutions` table instead of `references.resolved_*` columns.
    ///
    /// This preserves the global full-index resolution data while adding
    /// closure-scoped resolution that only applies within this focus closure.
    ///
    /// `visibility_filter` is an optional predicate `(symbol, from_file) -> bool`
    /// that returns true if a symbol is visible from the reference's file.
    /// Applied during strategy 6 (project-wide name search) to exclude symbols
    /// that exist in closure files but are not visible (e.g., C `static`
    /// functions, Rust `private` items).
    pub fn resolve_for_closure(
        &mut self,
        closure_id: &str,
        generation: i64,
        closure_files: &[FileId],
        visibility_filter: Option<&VisibilityFilterFn>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        self.resolve_for_closure_kinds(
            closure_id,
            generation,
            closure_files,
            visibility_filter,
            None,
        )
    }

    /// Closure-scoped resolution restricted to reference kinds required by
    /// the active graph strategies. `None` preserves the explicit full
    /// resolver behavior; an empty set performs no reference work.
    pub fn resolve_for_closure_kinds(
        &mut self,
        closure_id: &str,
        generation: i64,
        closure_files: &[FileId],
        visibility_filter: Option<&VisibilityFilterFn>,
        reference_kinds: Option<&HashSet<ReferenceKind>>,
    ) -> anyhow::Result<(Vec<(ReferenceUse, ResolvedTarget)>, ResolutionStats)> {
        let mut by_file: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
        for fid in closure_files {
            for r in self.store.find_references_by_file(fid)? {
                if reference_kinds.is_some_and(|kinds| !kinds.contains(&r.kind)) {
                    continue;
                }
                by_file.entry(r.file_id).or_default().push(r);
            }
        }

        let total_refs: usize = by_file.values().map(|v| v.len()).sum();

        let mut stats = ResolutionStats {
            total_refs,
            ..Default::default()
        };

        let mut resolved_pairs: Vec<(ReferenceUse, ResolvedTarget)> = Vec::new();
        let mut scoped_candidate_cache: HashMap<String, Vec<SymbolDef>> = HashMap::new();
        let mut scoped_file_path_cache: HashMap<FileId, String> = HashMap::new();
        let mut batch: Vec<StagedResolutionRow> = Vec::new();
        let batch_size = 500;

        for (file_id, refs) in &by_file {
            let ctx = match ResolutionContext::build(&self.store, *file_id) {
                Ok(c) => c,
                Err(e) => {
                    stats.add_warning(format!("failed to build context: {e}"));
                    continue;
                }
            };
            let preferred = self.import_resolver.collect_imported_file_ids(&ctx.imports);
            let source_is_test = is_explicit_test_path(&ctx.file.path);
            let source_parent = std::path::Path::new(&ctx.file.path)
                .parent()
                .map(|path| path.to_string_lossy().to_string());
            let mut scoped_state = ScopedResolutionState {
                context: &ctx,
                visibility_filter,
                preferred_files: preferred,
                source_is_test,
                source_parent,
                candidate_cache: &mut scoped_candidate_cache,
                file_path_cache: &mut scoped_file_path_cache,
            };

            for reference in refs {
                let target = self.resolve_one_scoped(reference, &mut scoped_state);
                match target {
                    Some(target) => {
                        let (resolution_scope, coverage_tier) = scope_and_tier(&target);
                        let semantic_confidence =
                            confidence_to_semantic(target.confidence.as_f32() as f64);
                        let provenance_str = format!(
                            "{}:{}:{}",
                            ctx.file.path,
                            target.strategy.as_str(),
                            target.provenance.as_str()
                        );

                        batch.push((
                            closure_id.to_string(),
                            generation,
                            reference.id.as_bytes().to_vec(),
                            resolution_scope.to_string(),
                            Some(target.symbol_id.as_bytes().to_vec()),
                            coverage_tier.to_string(),
                            semantic_confidence,
                            target.strategy.as_str().to_string(),
                            Some(provenance_str),
                        ));

                        resolved_pairs.push((reference.clone(), target.clone()));
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

            if batch.len() >= batch_size {
                if let Err(e) = self.store.batch_insert_reference_resolutions(&batch) {
                    stats.add_warning(format!("batch insert failed: {e}"));
                }
                batch.clear();
            }
        }

        if !batch.is_empty() {
            if let Err(e) = self.store.batch_insert_reference_resolutions(&batch) {
                stats.add_warning(format!("batch insert failed: {e}"));
            }
        }

        Ok((resolved_pairs, stats))
    }

    /// Resolve a single reference for scoped closure resolution.
    ///
    /// Same 6-strategy pipeline as [`resolve_one`], but strategy 6
    /// (project-wide name search) applies an optional visibility filter.
    fn resolve_one_scoped(
        &self,
        reference: &ReferenceUse,
        state: &mut ScopedResolutionState<'_>,
    ) -> Option<ResolvedTarget> {
        if is_builtin_reference(reference, state.context.file.language) {
            return None;
        }

        // Contextual strategies 2-5 are shared with resolve_one_core.
        if let Some(result) = resolve_contextual_strategies(
            reference,
            state.context,
            &self.import_resolver,
            &self.name_matcher,
        ) {
            return Some(result);
        }

        // Strategy 6: indexed exact-name lookup with closure-local caching.
        // Focus queries must not build a GlobalSymbolIndex over every project
        // symbol. Ambiguous fuzzy evidence remains unresolved in local mode.
        {
            let _timer = StrategyTimer::new(&S6_TIME_NS);
            let candidates = state
                .candidate_cache
                .entry(reference.name.clone())
                .or_insert_with(|| {
                    self.store
                        .find_symbols_by_name(&reference.name)
                        .unwrap_or_default()
                });
            let mut ranked = candidates
                .iter()
                .filter(|symbol| scoped_kind_is_compatible(reference.kind, symbol.kind))
                .filter(|symbol| {
                    state
                        .visibility_filter
                        .is_none_or(|filter| filter(symbol, reference.file_id))
                })
                .filter_map(|symbol| {
                    let candidate_path = state
                        .file_path_cache
                        .entry(symbol.file_id)
                        .or_insert_with(|| {
                            self.store
                                .get_file(&symbol.file_id)
                                .ok()
                                .flatten()
                                .map(|file| file.path)
                                .unwrap_or_default()
                        })
                        .clone();
                    if candidate_path.is_empty()
                        || (!state.source_is_test && is_explicit_test_path(&candidate_path))
                    {
                        return None;
                    }
                    let candidate_parent = std::path::Path::new(&candidate_path)
                        .parent()
                        .map(|path| path.to_string_lossy().to_string());
                    Some((
                        usize::from(
                            !state.preferred_files.is_empty()
                                && !state.preferred_files.contains(&symbol.file_id),
                        ),
                        proximity_tier(state.source_parent.as_ref(), candidate_parent.as_ref()),
                        candidate_path,
                        symbol.qualified_name.clone(),
                        symbol.id,
                    ))
                })
                .collect::<Vec<_>>();
            ranked.sort_unstable();
            if let Some((_, _, _, _, symbol_id)) = ranked.into_iter().next() {
                S6_COUNT.fetch_add(1, Ordering::Relaxed);
                S6_EXACT_COUNT.fetch_add(1, Ordering::Relaxed);
                return Some(ResolvedTarget {
                    symbol_id,
                    confidence: Confidence::certain(),
                    strategy: ResolutionStrategy::NameOnly,
                    provenance: Provenance::Heuristic,
                });
            }
        }
        MISS_COUNT.fetch_add(1, Ordering::Relaxed);
        None
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
        file_scope_cache: &std::sync::Mutex<HashMap<FileId, HashSet<FileId>>>,
    ) -> Option<ResolvedTarget> {
        resolve_one_core(
            reference,
            ctx,
            &self.import_resolver,
            &self.name_matcher,
            self.global_index.as_ref(),
            None,
            file_scope_cache,
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

/// Per-file timing for context build distribution analysis.
#[derive(Debug, Clone)]
struct ContextBuildTiming {
    file_id: FileId,
    refs_count: usize,
    symbols_count: usize,
    scopes_count: usize,
    imports_count: usize,
    elapsed_us: u64,
}

/// Timing telemetry collected during `resolve_all_parallel`.
#[derive(Debug, Default)]
struct ResolutionTelemetry {
    // Phase 0: Session build
    session_build_ms: u64,
    // Load unresolved refs from DB
    unresolved_load_ms: u64,
    unresolved_refs: usize,
    files_with_refs: usize,
    // Group by file
    group_by_file_ms: u64,
    // Dirty/clean split (includes per-file DB calls)
    dirty_clean_split_ms: u64,
    dirty_files: usize,
    clean_files: usize,
    dirty_refs: usize,
    clean_refs: usize,
    // Context builds (serial, dirty files only)
    context_build_ms: u64,
    context_build_timings: Vec<ContextBuildTiming>,
    context_build_failures: usize,
    // Parallel resolve wall times (overlap with writer)
    resolve_dirty_ms: u64,
    resolve_clean_ms: u64,
    // Writer thread
    writer_total_ms: u64,
    writer_batches: u64,
    writer_rows_written: u64,
    writer_rows_per_sec: f64,
    // Fingerprint update after resolution
    fingerprint_update_ms: u64,
}

/// Telemetry from the writer thread, returned alongside resolution results.
#[derive(Debug, Default)]
#[allow(dead_code)]
struct WriterTelemetry {
    total_ms: u64,
    batches: u64,
    rows_written: u64,
    rows_per_sec: f64,
    slow_batch_count: u64,
}

/// Log resolution telemetry summary at `target: "atlas_resolve"`.
fn log_resolution_telemetry(t: &ResolutionTelemetry) {
    tracing::info!(
        target: "atlas_resolve",
        session_build_ms = t.session_build_ms,
        unresolved_load_ms = t.unresolved_load_ms,
        unresolved_refs = t.unresolved_refs,
        files_with_refs = t.files_with_refs,
        group_by_file_ms = t.group_by_file_ms,
        dirty_clean_split_ms = t.dirty_clean_split_ms,
        dirty_files = t.dirty_files,
        clean_files = t.clean_files,
        dirty_refs = t.dirty_refs,
        clean_refs = t.clean_refs,
        context_build_ms = t.context_build_ms,
        context_build_failures = t.context_build_failures,
        resolve_dirty_ms = t.resolve_dirty_ms,
        resolve_clean_ms = t.resolve_clean_ms,
        writer_total_ms = t.writer_total_ms,
        writer_batches = t.writer_batches,
        writer_rows_written = t.writer_rows_written,
        writer_rows_per_sec = t.writer_rows_per_sec,
        fingerprint_update_ms = t.fingerprint_update_ms,
        "resolution.telemetry"
    );
}

/// Log context build distribution: p50/p95/max + top-10 slowest files.
fn log_context_build_distribution(timings: &[ContextBuildTiming]) {
    if timings.is_empty() {
        return;
    }
    let mut els: Vec<u64> = timings.iter().map(|t| t.elapsed_us).collect();
    els.sort_unstable();
    let n = els.len();
    let avg = els.iter().sum::<u64>() / n as u64;
    let p50 = els[n * 50 / 100];
    let p95 = els[n.saturating_sub(1).min(n * 95 / 100)];
    let max = els[n - 1];

    tracing::info!(
        target: "atlas_resolve",
        count = n,
        avg_us = avg,
        p50_us = p50,
        p95_us = p95,
        max_us = max,
        "resolution.context_build.distribution"
    );

    // Top 10 slowest
    let mut with_file: Vec<&ContextBuildTiming> = timings.iter().collect();
    with_file.sort_by_key(|t| std::cmp::Reverse(t.elapsed_us));
    for t in with_file.iter().take(10) {
        tracing::info!(
            target: "atlas_resolve",
            file_id = %t.file_id,
            refs = t.refs_count,
            symbols = t.symbols_count,
            scopes = t.scopes_count,
            imports = t.imports_count,
            elapsed_us = t.elapsed_us,
            "resolution.context_build.top10"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use extraction::create_frontend;
    use extraction::{ExtractionMode, extract_file_with_mode};
    use graph::{GraphBuilder, GraphEngine};
    use std::path::PathBuf;
    use types::FileFacts;

    fn extract_full(
        frontend: &extraction::LanguageFrontend,
        file_id: FileId,
        path: &std::path::Path,
        source: &str,
        content_hash: &str,
    ) -> anyhow::Result<FileFacts> {
        extract_file_with_mode(
            frontend,
            file_id,
            path,
            source,
            content_hash,
            ExtractionMode::Full,
            &(),
        )
    }

    #[test]
    fn closure_resolution_does_not_build_the_project_wide_symbol_index() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let mut resolver = ReferenceResolver::new(store);

        resolver
            .resolve_for_closure("test_closure", 0, &[], None)
            .unwrap();

        assert!(resolver.global_index.is_none());
    }

    #[test]
    fn project_resolution_builds_the_global_index_for_unimported_cross_file_calls() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let frontend = create_frontend(Language::C).unwrap();
        let target_id = FileId::generate("target.c");
        let caller_id = FileId::generate("caller.c");
        let target = extract_full(
            &frontend,
            target_id,
            &PathBuf::from("target.c"),
            "void external_fn(void) {}",
            "target",
        )
        .unwrap();
        let caller = extract_full(
            &frontend,
            caller_id,
            &PathBuf::from("caller.c"),
            "void caller(void) { external_fn(); }",
            "caller",
        )
        .unwrap();
        store.insert_file_facts(&target).unwrap();
        store.insert_file_facts(&caller).unwrap();

        let target_symbol = target
            .symbols
            .iter()
            .find(|symbol| symbol.name == "external_fn")
            .unwrap();
        let mut resolver = ReferenceResolver::new(store.clone());
        let (resolved, _) = resolver
            .resolve_all_parallel(store.clone(), None, None)
            .unwrap();

        assert!(resolved.iter().any(|(reference, resolved_target)| {
            reference.name == "external_fn" && resolved_target.symbol_id == target_symbol.id
        }));
    }

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

    #[test]
    fn call_references_skip_edit_distance_fuzzy_fallback() {
        let file_id = FileId::generate("test.c");
        let name = "inet_iif".to_string();
        let reference = ReferenceUse {
            id: ReferenceId::generate(&file_id, None, 0, 8, &name, ReferenceKind::Call),
            file_id,
            source_symbol: None,
            scope_id: None,
            kind: ReferenceKind::Call,
            text: name.clone(),
            name,
            receiver: None,
            arity: None,
            range: TextRange {
                start_byte: 0,
                end_byte: 8,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 8,
            },
            binding_id: None,
            resolved: None,
        };

        assert!(should_run_fuzzy_fallback(&reference.name));
        assert!(!should_run_fuzzy_fallback_for_reference(&reference));
    }

    /// Verify that `scope_and_tier` returns snake_case strings that match
    /// what `build_incoming_precision` in the focus graph builder expects.
    #[test]
    fn test_scope_and_tier_returns_snake_case() {
        let dummy_symbol_id = SymbolId::generate(
            &FileId::generate("test.ts"),
            "typescript",
            "test",
            "function",
            None,
        );
        let confidence = Confidence::new(0.95);

        // ExactMatch → closure_complete
        let exact = ResolvedTarget {
            symbol_id: dummy_symbol_id,
            confidence,
            strategy: ResolutionStrategy::ExactMatch,
            provenance: Provenance::TreeSitter,
        };
        let (scope, tier) = scope_and_tier(&exact);
        assert_eq!(
            scope, "closure_complete",
            "ExactMatch scope should be snake_case"
        );
        assert_eq!(
            tier, "closure_complete",
            "ExactMatch tier should be snake_case"
        );

        // ImportResolved → closure_complete
        let import = ResolvedTarget {
            symbol_id: dummy_symbol_id,
            confidence,
            strategy: ResolutionStrategy::ImportResolved,
            provenance: Provenance::TreeSitter,
        };
        let (scope, tier) = scope_and_tier(&import);
        assert_eq!(scope, "closure_complete");
        assert_eq!(tier, "closure_complete");

        // NameOnly → boundary
        let name_only = ResolvedTarget {
            symbol_id: dummy_symbol_id,
            confidence,
            strategy: ResolutionStrategy::NameOnly,
            provenance: Provenance::TreeSitter,
        };
        let (scope, tier) = scope_and_tier(&name_only);
        assert_eq!(scope, "boundary");
        assert_eq!(tier, "boundary");

        // FuzzyMatch → boundary
        let fuzzy = ResolvedTarget {
            symbol_id: dummy_symbol_id,
            confidence,
            strategy: ResolutionStrategy::FuzzyMatch,
            provenance: Provenance::TreeSitter,
        };
        let (scope, tier) = scope_and_tier(&fuzzy);
        assert_eq!(scope, "boundary");
        assert_eq!(tier, "boundary");

        // Heuristic → boundary
        let heuristic = ResolvedTarget {
            symbol_id: dummy_symbol_id,
            confidence,
            strategy: ResolutionStrategy::Heuristic,
            provenance: Provenance::Heuristic,
        };
        let (scope, tier) = scope_and_tier(&heuristic);
        assert_eq!(scope, "boundary");
        assert_eq!(tier, "boundary");
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
        let lib_facts = extract_full(
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
        let main_facts = extract_full(
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
        let (resolved, stats) = resolver
            .resolve_all_parallel(store.clone(), None, None)
            .expect("resolution failed");

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
    fn arkts_framework_references_are_terminal_before_global_resolution() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let js = create_frontend(Language::JavaScript).unwrap();
        let js_facts = extract_full(
            &js,
            FileId::generate("dist/common.js"),
            &PathBuf::from("dist/common.js"),
            "export function $r(value) { return value; }\nexport function Component(value) { return value; }",
            "js",
        )
        .unwrap();
        store.insert_file_facts(&js_facts).unwrap();

        let arkts = create_frontend(Language::ArkTS).unwrap();
        let arkts_facts = extract_full(
            &arkts,
            FileId::generate("Main.ets"),
            &PathBuf::from("Main.ets"),
            "@Component\nstruct View { build() {} }\nexport function render() { return $r('sys.color.background'); }",
            "arkts",
        )
        .unwrap();
        assert!(
            arkts_facts.references.iter().any(|reference| {
                reference.kind == ReferenceKind::Call && reference.name == "$r"
            })
        );
        assert!(arkts_facts.references.iter().any(|reference| {
            reference.kind == ReferenceKind::Decoration && reference.name == "Component"
        }));
        store.insert_file_facts(&arkts_facts).unwrap();

        let mut resolver = ReferenceResolver::new(store.clone());
        let (resolved, _) = resolver
            .resolve_all_parallel(store, None, None)
            .expect("resolution");
        assert!(!resolved.iter().any(|(reference, _)| {
            reference.file_id == arkts_facts.file.file_id
                && matches!(reference.name.as_str(), "$r" | "Component")
        }));
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
        let lib_facts = extract_full(
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
        let main_facts = extract_full(
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
        let (resolved, _) = resolver
            .resolve_all_parallel(store.clone(), None, None)
            .expect("resolution failed");

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

    /// C++ qualified call `CertUtils::GetDev()` must resolve and create a Calls edge
    /// (requires re-index after extraction query change; not a legacy-DB path).
    #[test]
    fn test_cpp_qualified_call_creates_calls_edge() {
        let source = r#"
class CertUtils {
public:
    static int GetDev();
};

int CertUtils::GetDev() {
    return 1;
}

int use_dev() {
    return CertUtils::GetDev();
}
"#;
        let file_id = FileId::generate("cert_utils.cpp");
        let frontend = create_frontend(Language::Cpp).expect("cpp frontend");
        let facts = extract_full(
            &frontend,
            file_id,
            &PathBuf::from("cert_utils.cpp"),
            source,
            "abc",
        )
        .expect("cpp extract");

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&facts).expect("insert");

        let mut resolver = ReferenceResolver::new(Arc::clone(&store));
        let (resolved, stats) = resolver
            .resolve_all_parallel(store.clone(), None, None)
            .expect("resolve");
        assert!(
            stats.resolved > 0,
            "expected resolved refs, got stats={stats:?}"
        );

        // Call ref simple name + resolved target.
        let call_resolved = resolved
            .iter()
            .any(|(r, _)| r.kind == ReferenceKind::Call && r.name == "GetDev");
        assert!(
            call_resolved,
            "expected resolved Call ref name=GetDev; resolved={:?}",
            resolved
                .iter()
                .filter(|(r, _)| r.kind == ReferenceKind::Call)
                .map(|(r, t)| (&r.name, &r.text, t.symbol_id))
                .collect::<Vec<_>>()
        );

        let builder = GraphBuilder::new(Arc::clone(&store));
        let build_stats = builder.build_all(&resolved);
        assert!(
            build_stats.edges_built > 0,
            "expected edges, got {build_stats:?}"
        );

        let graph = GraphEngine::from_store(store.as_ref(), 0.0).expect("graph build failed");
        let get_dev = store
            .find_symbols_by_qname("CertUtils::GetDev")
            .unwrap()
            .into_iter()
            .next()
            .expect("GetDev symbol");
        let callers = graph.callers(&get_dev.id);
        let caller_names: Vec<&str> = callers
            .callers
            .iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            caller_names.contains(&"use_dev"),
            "expected use_dev to call CertUtils::GetDev, got {caller_names:?}"
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
        let lib_facts = extract_full(
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
        let main_facts = extract_full(
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
        let (resolved, _stats) = resolver
            .resolve_all_parallel(store.clone(), None, None)
            .expect("resolution failed");

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
        let subscriber = tracing_subscriber::fmt().with_test_writer().finish();
        tracing::subscriber::with_default(subscriber, || {
            let store = Arc::new(Store::open_in_memory().unwrap());
            store.init_schema().unwrap();

            let lib_src =
                r#"export function greet(name: string): string { return 'Hello, ' + name; }"#;
            let main_src = r#"import { greet } from './lib';
function main() {
    greet("World");
}
main();
"#;
            let ts = create_frontend(Language::TypeScript).unwrap();

            let lib_id = FileId::generate("lib.ts");
            let lib = extract_full(&ts, lib_id, &PathBuf::from("lib.ts"), lib_src, "abc").unwrap();
            store.insert_file_facts(&lib).unwrap();

            let main_id = FileId::generate("main.ts");
            let main =
                extract_full(&ts, main_id, &PathBuf::from("main.ts"), main_src, "abc").unwrap();
            store.insert_file_facts(&main).unwrap();

            let mut resolver = ReferenceResolver::new(Arc::clone(&store));
            let (_resolved, stats) = resolver
                .resolve_all_parallel(store.clone(), None, None)
                .unwrap();
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

    /// Regression: after callsite.callee denormalization was eliminated,
    /// `find_resolved_callsites_by_callee()` must return deterministic results
    /// from fresh stores resolved through the project-wide parallel path.
    #[test]
    fn resolved_callsites_consistent_across_parallel_runs() {
        let ts = create_frontend(Language::TypeScript).unwrap();
        let lib_id = FileId::generate("lib.ts");
        let main_id = FileId::generate("main.ts");

        let lib_src = r#"export function helper(x: number): number { return x * 2; }
export function other(): string { return "unrelated"; }
"#;
        let main_src = r#"import { helper } from './lib';
function main() { return helper(21); }
function other() { return "no call"; }
"#;

        let extract = |fid: &FileId, src: &str, path: &str| -> FileFacts {
            extract_full(&ts, *fid, &PathBuf::from(path), src, "hash").unwrap()
        };

        // ── Run A ──
        let store_a = Arc::new(Store::open_in_memory().unwrap());
        store_a.init_schema().unwrap();
        let lib_facts_a = extract(&lib_id, lib_src, "lib.ts");
        let main_facts_a = extract(&main_id, main_src, "main.ts");
        store_a.insert_file_facts(&lib_facts_a).unwrap();
        store_a.insert_file_facts(&main_facts_a).unwrap();

        let mut resolver_a = ReferenceResolver::new(store_a.clone());
        let (_resolved_a, stats_a) = resolver_a
            .resolve_all_parallel(store_a.clone(), None, None)
            .unwrap();
        assert!(
            stats_a.resolved > 0,
            "first resolution should resolve at least one reference"
        );

        let callee_id = store_a
            .find_symbols_by_qname("helper")
            .unwrap()
            .first()
            .unwrap()
            .id;

        let callsites_a = store_a
            .find_resolved_callsites_by_callee(&callee_id)
            .unwrap();

        // ── Run B ──
        let store_b = Arc::new(Store::open_in_memory().unwrap());
        store_b.init_schema().unwrap();
        let lib_facts_b = extract(&lib_id, lib_src, "lib.ts");
        let main_facts_b = extract(&main_id, main_src, "main.ts");
        store_b.insert_file_facts(&lib_facts_b).unwrap();
        store_b.insert_file_facts(&main_facts_b).unwrap();

        let mut resolver_b = ReferenceResolver::new(store_b.clone());
        let (_resolved_b, stats_b) = resolver_b
            .resolve_all_parallel(store_b.clone(), None, None)
            .unwrap();
        assert!(
            stats_b.resolved > 0,
            "parallel resolution should resolve at least one reference"
        );

        let callsites_b = store_b
            .find_resolved_callsites_by_callee(&callee_id)
            .unwrap();

        // ── Assertions ──
        assert!(
            !callsites_a.is_empty(),
            "first run should find callsites targeting helper()"
        );
        assert!(
            !callsites_b.is_empty(),
            "second run should find callsites targeting helper()"
        );
        assert_eq!(
            callsites_a.len(),
            callsites_b.len(),
            "both runs should find the same number of callsites targeting helper()"
        );

        for (a, b) in callsites_a.iter().zip(callsites_b.iter()) {
            assert_eq!(a.callsite.id, b.callsite.id, "callsite ids should match");
            assert_eq!(
                a.callsite.caller, b.callsite.caller,
                "callsite callers should match"
            );
            assert_eq!(a.callee, b.callee, "callee symbols should match");
        }
    }

    /// Smoke test: `resolve_all_parallel` runs with internal telemetry
    /// collection and does NOT panic. The telemetry is logged via tracing
    /// but not returned to the caller — observable via tracing subscriber
    /// in integration tests.
    #[test]
    fn test_resolution_telemetry_populated() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let ts = create_frontend(Language::TypeScript).unwrap();

        let lib_src = r#"export function greet(name: string): string { return 'Hello, ' + name; }"#;
        let main_src = r#"import { greet } from './lib';
function main() {
    greet("World");
}
main();
"#;

        let lib_id = FileId::generate("lib.ts");
        let lib = extract_full(&ts, lib_id, &PathBuf::from("lib.ts"), lib_src, "abc").unwrap();
        store.insert_file_facts(&lib).unwrap();

        let main_id = FileId::generate("main.ts");
        let main = extract_full(&ts, main_id, &PathBuf::from("main.ts"), main_src, "abc").unwrap();
        store.insert_file_facts(&main).unwrap();

        let _ = (lib_id, main_id);

        let mut resolver = ReferenceResolver::new(store.clone());
        let (resolved, stats) = resolver
            .resolve_all_parallel(store, None, None)
            .expect("resolve_all_parallel failed");
        assert!(
            stats.resolved > 0,
            "expected at least 1 resolved reference, got {}",
            stats.resolved
        );
        assert!(
            !resolved.is_empty(),
            "expected resolved pairs to be non-empty"
        );
    }

    /// Unit test: context build timing is collected internally during
    /// `resolve_all_parallel`. Telemetry is observable via tracing
    /// subscriber in integration tests.
    #[test]
    fn test_context_build_timing_collected() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let ts = create_frontend(Language::TypeScript).unwrap();

        let lib_src = r#"export function greet(name: string): string { return 'Hello, ' + name; }"#;
        let main_src = r#"import { greet } from './lib';
function main() {
    greet("World");
}
main();
"#;

        let lib_id = FileId::generate("lib.ts");
        let lib = extract_full(&ts, lib_id, &PathBuf::from("lib.ts"), lib_src, "abc").unwrap();
        store.insert_file_facts(&lib).unwrap();

        let main_id = FileId::generate("main.ts");
        let main = extract_full(&ts, main_id, &PathBuf::from("main.ts"), main_src, "abc").unwrap();
        store.insert_file_facts(&main).unwrap();

        let _ = (lib_id, main_id);

        let mut resolver = ReferenceResolver::new(store.clone());
        let (_resolved, stats) = resolver
            .resolve_all_parallel(store, None, None)
            .expect("resolve_all_parallel should succeed");
        assert!(stats.resolved > 0, "should resolve cross-file import");
    }

    /// The `WriterTelemetry` struct is internal and returned from the writer
    /// thread — this test verifies the parallel resolve path completes
    /// without panicking (compile-time check for WriterTelemetry in the
    /// return tuple).
    #[test]
    fn test_writer_telemetry_in_parallel_resolve() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let ts = create_frontend(Language::TypeScript).unwrap();

        let lib_src = r#"export function helper(x: number): number { return x * 2; }
export function other(): string { return "unrelated"; }
"#;
        let main_src = r#"import { helper } from './lib';
function main() { return helper(21); }
function other() { return "no call"; }
"#;

        let lib_id = FileId::generate("lib.ts");
        let lib = extract_full(&ts, lib_id, &PathBuf::from("lib.ts"), lib_src, "hash").unwrap();
        store.insert_file_facts(&lib).unwrap();

        let main_id = FileId::generate("main.ts");
        let main = extract_full(&ts, main_id, &PathBuf::from("main.ts"), main_src, "hash").unwrap();
        store.insert_file_facts(&main).unwrap();

        let _ = (lib_id, main_id);

        let mut resolver = ReferenceResolver::new(store.clone());
        let (_resolved, stats) = resolver
            .resolve_all_parallel(store, None, None)
            .expect("resolve_all_parallel should succeed");
        assert!(stats.resolved > 0, "should resolve cross-file import");
    }
}
