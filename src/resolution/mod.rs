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

        for (file_id, refs) in &by_file {
            // Build resolution context (loads all symbols/scopes/imports once)
            let ctx = match ResolutionContext::build(&self.store, *file_id) {
                Ok(c) => c,
                Err(e) => {
                    // Log but continue with other files
                    eprintln!("Warning: failed to build context for file: {}", e);
                    continue;
                }
            };

            for reference in refs {
                match self.resolve_one(reference, &ctx) {
                    Some(target) => {
                        // Update reference with resolved target
                        if let Err(e) = self
                            .store
                            .update_reference_resolution(&reference.id, &target)
                        {
                            eprintln!(
                                "Warning: failed to update resolution for {}: {}",
                                reference.name, e
                            );
                            continue;
                        }
                        stats.resolved += 1;
                        *stats
                            .by_strategy
                            .entry(target.strategy.as_str().to_string())
                            .or_default() += 1;

                        // Create structural edges from this resolution
                        let edges = self.create_edges(reference, &target, &ctx);
                        if !edges.is_empty() {
                            if let Err(e) = self.store.insert_edges(&edges) {
                                eprintln!("Warning: failed to insert edges: {}", e);
                            } else {
                                stats.edges_created += edges.len();
                            }
                        }
                    }
                    None => {
                        stats.unresolved += 1;
                    }
                }
            }
        }

        Ok(stats)
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
    /// - `References` for non-call references
    fn create_edges(
        &self,
        reference: &ReferenceUse,
        target: &ResolvedTarget,
        ctx: &ResolutionContext,
    ) -> Vec<RawEdge> {
        let mut edges = Vec::new();

        let target_sym = match ctx.symbols_by_id.get(&target.symbol_id) {
            Some(s) => s,
            None => return edges,
        };

        // Source is the enclosing function/class that contains the reference
        let source = match reference.source_symbol {
            Some(s) => s,
            None => return edges,
        };

        let edge_kind = if reference.kind == ReferenceKind::Call {
            match target_sym.kind {
                SymbolKind::Class | SymbolKind::Struct => EdgeKind::Instantiates,
                SymbolKind::Interface | SymbolKind::Trait => EdgeKind::Implements,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => EdgeKind::Calls,
                _ => return edges, // Non-callable target — no structural edge
            }
        } else {
            EdgeKind::References
        };

        let edge = RawEdge {
            id: EdgeId::generate(
                &source,
                &target.symbol_id,
                edge_kind.as_str(),
                Some(&reference.id),
                target.provenance.as_str(),
            ),
            source,
            target: target.symbol_id,
            kind: edge_kind,
            confidence: target.confidence,
            provenance: target.provenance,
        };

        edges.push(edge);

        // Also create Contains edges from container symbols during resolution
        if let Some(container) = target_sym.container {
            if let Some(_container_sym) = ctx.symbols_by_id.get(&container) {
                let contains_edge = RawEdge {
                    id: EdgeId::generate(
                        &container,
                        &target.symbol_id,
                        EdgeKind::Contains.as_str(),
                        None::<&ReferenceId>,
                        Provenance::TreeSitter.as_str(),
                    ),
                    source: container,
                    target: target.symbol_id,
                    kind: EdgeKind::Contains,
                    confidence: Confidence::certain(),
                    provenance: Provenance::TreeSitter,
                };
                edges.push(contains_edge);
            }
        }

        edges
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
}
