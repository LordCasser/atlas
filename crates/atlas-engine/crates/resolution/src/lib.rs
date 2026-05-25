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

use db::Store;
use types::*;

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

/// Three-stage reference resolution orchestrator.
///
/// P2: `resolve_all()` only resolves references and updates the `"references"`
/// table. Edge creation is delegated to `GraphBuilder`.
///
/// P4: Uses `GlobalSymbolIndex` for project-wide name search instead of
/// per-reference FTS5 queries. The global index is built once at the start
/// of resolution.
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
    ///
    /// When `PathAliasResolver` is configured (e.g. `@/utils` → `src/utils`),
    /// import path resolution will apply aliases before generating candidate
    /// qualified names.
    pub fn with_path_alias(store: Arc<Store>, path_alias: PathAliasResolver) -> Self {
        Self {
            import_resolver: ImportResolver::with_path_alias(store.clone(), path_alias),
            name_matcher: NameMatcher::new(),
            store,
            global_index: None,
        }
    }

    /// Resolve all unresolved references in the project.
    ///
    /// Returns `(resolved, stats)` where:
    /// - `resolved` contains `(ReferenceUse, ResolvedTarget)` pairs for use
    ///   by `GraphBuilder` to create structural edges.
    /// - `stats` contains resolution statistics.
    ///
    /// P4: Loads `GlobalSymbolIndex` once, then processes file groups with
    /// O(1) in-memory lookups instead of per-reference FTS5 queries.
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
            // Build resolution context (loads all symbols/scopes/imports once)
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

            // Flush accumulated resolutions when batch is full
            if pending_resolutions.len() >= batch_size {
                self.flush_resolutions(&mut pending_resolutions, &mut stats);
            }
        }

        // Final flush
        self.flush_resolutions(&mut pending_resolutions, &mut stats);

        Ok((all_resolved, stats))
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
        // ---- Strategy 1: Built-in / external filter ----
        // Builtins are filtered out entirely — there is no real symbol in the DB
        // to resolve against, and creating a ResolvedTarget with SymbolId::default()
        // would produce edges with invalid ghost targets.
        if BuiltinFilter::is_builtin(&reference.name, ctx.file.language) {
            return None;
        }

        // ---- Strategy 2: Scope-local exact match ----
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

        // ---- Strategy 3: Container/class-local ----
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

        // ---- Strategy 4: Same-file exact match ----
        let same_file = ctx.find_in_file_by_name(&reference.name);
        if let Some(matched) =
            self.name_matcher
                .best_match(&same_file, &reference.name, Confidence::certain())
        {
            return Some(ResolvedTarget {
                symbol_id: matched.symbol_id,
                confidence: matched.confidence,
                strategy: matched.strategy,
                provenance: matched.provenance,
            });
        }

        // ---- Strategy 5: Import/include resolution ----
        for import in &ctx.imports {
            if let Ok(candidates) = self.import_resolver.resolve_import(import) {
                // Try re-export chain walking first (barrel files)
                if let Ok(chain_candidates) = self
                    .import_resolver
                    .resolve_through_reexports(import, candidates)
                {
                    if let Some(matched) = self.name_matcher.best_match(
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

        // ---- Strategy 6: Project-wide name search (P4: in-memory index) ----
        if let Some(ref idx) = self.global_index {
            let candidates = idx.find_by_name(&reference.name);
            if !candidates.is_empty() {
                if let Some(matched) =
                    self.name_matcher
                        .best_match(&candidates, &reference.name, Confidence::new(0.6))
                {
                    return Some(ResolvedTarget {
                        symbol_id: matched.symbol_id,
                        confidence: matched.confidence,
                        strategy: ResolutionStrategy::FuzzyMatch,
                        provenance: Provenance::Heuristic,
                    });
                }
            }
            // Bounded fuzzy fallback
            let fuzzy = idx.fuzzy_search(&reference.name, 2);
            if !fuzzy.is_empty() {
                if let Some(matched) =
                    self.name_matcher
                        .best_match(&fuzzy, &reference.name, Confidence::new(0.4))
                {
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
}
