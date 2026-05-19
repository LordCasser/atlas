//! Resolution layer: import resolver, name matcher, framework resolvers.
//!
//! Three-stage reference resolution pipeline:
//! 1. Scope-local exact match
//! 2. Import / include resolution
//! 3. Name-based fuzzy fallback (project-wide)
//!
//! Cross-module invariant: references are NEVER deleted — resolution updates
//! their `resolved` field in place but leaves the record intact.

use std::collections::HashMap;
use std::sync::Arc;

use crate::db::Store;
use crate::types::*;

use self::builtins::BuiltinFilter;
use self::context::ResolutionContext;
use self::import_resolver::ImportResolver;
use self::name_matcher::NameMatcher;

pub mod context;
pub mod import_resolver;
pub mod name_matcher;
pub mod builtins;
pub mod frameworks;

/// Three-stage reference resolution orchestrator.
pub struct ReferenceResolver {
    store: Arc<Store>,
    import_resolver: ImportResolver,
    name_matcher: NameMatcher,
}

impl ReferenceResolver {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            import_resolver: ImportResolver::new(store.clone()),
            name_matcher: NameMatcher::new(),
            store,
        }
    }

    /// Resolve all unresolved references in the project.
    ///
    /// Uses batched writes: resolution results and edges are accumulated in
    /// memory and flushed to SQLite in bulk transactions. This avoids the
    /// per-reference transaction overhead that dominated indexing time for
    /// large projects (e.g., 50s+ for curl's 95k+ edges).
    pub fn resolve_all(&self) -> anyhow::Result<ResolutionStats> {
        let unresolved = self.store.find_unresolved_references()?;
        let total_refs = unresolved.len();
        let mut stats = ResolutionStats::default();
        stats.total_refs = total_refs;

        // Group references by file for efficient context loading
        let mut by_file: HashMap<FileId, Vec<ReferenceUse>> = HashMap::new();
        for r in &unresolved {
            by_file.entry(r.file_id).or_default().push(r.clone());
        }

        // Accumulate resolutions and edges for batched writes
        let mut pending_resolutions: Vec<(ReferenceId, ResolvedTarget)> = Vec::new();
        let mut pending_edges: Vec<RawEdge> = Vec::new();
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
                        stats.resolved += 1;
                        *stats
                            .by_strategy
                            .entry(target.strategy.as_str().to_string())
                            .or_default() += 1;

                        // Create structural edges from this resolution
                        match self.create_edges(reference, &target) {
                            Ok(edges) => {
                                pending_edges.extend(edges);
                            }
                            Err(e) => {
                                stats.add_warning(format!("failed to create edges: {}", e));
                            }
                        }
                    }
                    None => {
                        stats.unresolved += 1;
                    }
                }
            }

            // Flush accumulated results when batch is full
            if pending_resolutions.len() >= batch_size {
                self.flush_batch(&mut pending_resolutions, &mut pending_edges, &mut stats);
            }
        }

        // Final flush
        self.flush_batch(&mut pending_resolutions, &mut pending_edges, &mut stats);

        Ok(stats)
    }

    /// Flush pending resolutions and edges to the store in batch.
    fn flush_batch(
        &self,
        pending_resolutions: &mut Vec<(ReferenceId, ResolvedTarget)>,
        pending_edges: &mut Vec<RawEdge>,
        stats: &mut ResolutionStats,
    ) {
        if !pending_resolutions.is_empty() {
            if let Err(e) = self.store.batch_update_resolutions(pending_resolutions) {
                stats.add_warning(format!("batch resolution update failed: {}", e));
            }
            pending_resolutions.clear();
        }
        if !pending_edges.is_empty() {
            let edge_count = pending_edges.len();
            if let Err(e) = self.store.batch_insert_edges(pending_edges) {
                stats.add_warning(format!("batch edge insert failed ({} edges): {}", edge_count, e));
            } else {
                stats.edges_created += edge_count;
            }
            pending_edges.clear();
        }
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
        if let Some(matched) = self.name_matcher.best_match(
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

        // ---- Strategy 5: Import/include resolution ----
        for import in &ctx.imports {
            if let Ok(candidates) = self.import_resolver.resolve_import(import) {
                if let Some(matched) = self.name_matcher.best_match(
                    &candidates,
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

        // ---- Strategy 6: Project-wide name search (FTS5) ----
        if let Ok(candidates) = self.store.search_symbols(&reference.name) {
            if let Some(matched) = self
                .name_matcher
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

        None
    }

    /// Create structural edges from a resolved reference.
    ///
    /// Produces:
    /// - `Calls` when a call reference resolves to a function/method/constructor
    /// - `Instantiates` when a call reference resolves to a class/struct
    /// - `Implements` when a call reference resolves to an interface/trait
    /// - `Extends` when an inheritance reference resolves to a class
    /// - `Implements` when an implementation reference resolves to an interface/trait
    /// - `References` for non-call references
    /// - `References` for non-call references
    ///
    /// Uses `self.store` to look up the target symbol (supports cross-file targets).
    fn create_edges(
        &self,
        reference: &ReferenceUse,
        target: &ResolvedTarget,
    ) -> anyhow::Result<Vec<RawEdge>> {
        let mut edges = Vec::new();

        // Look up target symbol from the DB (supports cross-file targets)
        let target_sym = match self.store.find_symbol_by_id(&target.symbol_id)? {
            Some(s) => s,
            None => return Ok(edges),
        };

        // Source is the enclosing function/class that contains the reference
        let source = match reference.source_symbol {
            Some(s) => s,
            None => return Ok(edges),
        };

        let edge_kind = if reference.kind == ReferenceKind::Call {
            match target_sym.kind {
                SymbolKind::Class | SymbolKind::Struct => EdgeKind::Instantiates,
                SymbolKind::Interface | SymbolKind::Trait => EdgeKind::Implements,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => EdgeKind::Calls,
                _ => return Ok(edges), // Non-callable target — no structural edge
            }
        } else if reference.kind == ReferenceKind::Inheritance {
            EdgeKind::Extends
        } else if reference.kind == ReferenceKind::Implementation {
            EdgeKind::Implements
        } else {
            EdgeKind::References
        };

        let mut edge = RawEdge::new(
            EdgeId::generate(
                &source,
                &target.symbol_id,
                edge_kind.as_str(),
                Some(&reference.id),
                target.provenance.as_str(),
            ),
            source,
            target.symbol_id,
            edge_kind,
            target.confidence,
            target.provenance,
        );
        edge.ref_id = Some(reference.id);
        edge.resolved_by = Some(target.strategy);

        edges.push(edge);

        // Also create Contains edges from container symbols during resolution
        if let Some(container) = target_sym.container {
            if self.store.find_symbol_by_id(&container)?.is_some() {
                let mut contains_edge = RawEdge::new(
                    EdgeId::generate(
                        &container,
                        &target.symbol_id,
                        EdgeKind::Contains.as_str(),
                        Some(&reference.id),
                        Provenance::TreeSitter.as_str(),
                    ),
                    container,
                    target.symbol_id,
                    EdgeKind::Contains,
                    Confidence::certain(),
                    Provenance::TreeSitter,
                );
                contains_edge.ref_id = Some(reference.id);
                contains_edge.resolved_by = Some(target.strategy);
                edges.push(contains_edge);
            }
        }

        Ok(edges)
    }
}

/// Statistics from a resolution run.
#[derive(Debug, Clone, Default)]
pub struct ResolutionStats {
    pub total_refs: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub by_strategy: HashMap<String, usize>,
    pub edges_created: usize,
    /// Non-fatal warnings collected during resolution (context build failures,
    /// edge insertion errors, etc.).
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
    use crate::db::Store;
    use crate::extraction::languages::typescript::TypeScriptAdapter;
    use crate::extraction::extract_file;
    use std::path::PathBuf;

    /// Verifies that cross-file import → call creates a structural Calls edge.
    #[test]
    fn test_cross_file_import_call_creates_edge() {
        // ── File 1: lib.ts — exports a function ──
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let lib_adapter = TypeScriptAdapter;
        let lib_facts = extract_file(
            &lib_adapter, lib_id, &PathBuf::from("lib.ts"), lib_src, "abc",
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
            &TypeScriptAdapter, main_id, &PathBuf::from("main.ts"), main_src, "abc",
        )
        .expect("main.ts extraction failed");

        // ── Store and index ──
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store.insert_file_facts(&main_facts).expect("insert main.ts");

        // ── Resolve ──
        let resolver = ReferenceResolver::new(Arc::new(store));
        let stats = resolver.resolve_all().expect("resolution failed");

        // Verify resolution happened
        assert!(stats.resolved > 0, "expected at least 1 resolved reference, got {}", stats.resolved);

        // Verify at least one Calls edge was created
        assert!(
            stats.edges_created > 0,
            "expected cross-file Calls edges, got {} edges",
            stats.edges_created
        );
    }

    #[test]
    fn test_cross_file_callers_callees_graph() {
        use crate::graph::GraphEngine;
        use crate::extraction::extract_file;
        use crate::extraction::languages::typescript::TypeScriptAdapter;

        // ── File 1: lib.ts — exports greet and farewell ──
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}

export function farewell(name: string): string {
    return `Goodbye, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let lib_adapter = TypeScriptAdapter;
        let lib_facts = extract_file(
            &lib_adapter, lib_id, &PathBuf::from("lib.ts"), lib_src, "abc",
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
            &TypeScriptAdapter, main_id, &PathBuf::from("main.ts"), main_src, "abc",
        )
        .expect("main.ts extraction failed");

        // ── Store, index, resolve ──
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store.insert_file_facts(&main_facts).expect("insert main.ts");

        let resolver = ReferenceResolver::new(Arc::clone(&store));
        resolver.resolve_all().expect("resolution failed");

        // ── Build graph and verify callers/callees ──
        let graph = GraphEngine::from_store(&store, 0.0).expect("graph build failed");

        // Find greet and main by qualified name
        let greet_id = store.find_symbols_by_qname("greet").unwrap()
            .first().unwrap().id;
        let main_id = store.find_symbols_by_qname("main").unwrap()
            .first().unwrap().id;

        // Verify callers: main → greet
        let greet_callers = graph.callers(&greet_id);
        let caller_names: Vec<&str> = greet_callers.callers.iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            caller_names.contains(&"main"),
            "expected main to be caller of greet, got: {:?}", caller_names
        );

        // Verify callees: main → greet
        let main_callees = graph.callees(&main_id);
        let callee_names: Vec<&str> = main_callees.callees.iter()
            .map(|ix| graph.snapshot().node(*ix).name.as_str())
            .collect();
        assert!(
            callee_names.contains(&"greet"),
            "expected greet to be callee of main, got: {:?}", callee_names
        );
    }
}
