//! GraphBuilder — constructs symbol-level edges from resolved references.
//!
//! Separated from ReferenceResolver (P2): the resolver only produces
//! `(ReferenceUse, ResolvedTarget)` pairs; this module converts those
//! resolved facts into `RawEdge` objects and writes them to the store.
//!
//! Edge kinds produced (symbol-level only):
//! - Calls, Instantiates, Implements (from Call references)
//! - Extends (from Inheritance references)
//! - Implements (from Implementation references)
//! - References (from all other reference kinds)
//! - Contains (from container → child relationships discovered during resolution)

use std::sync::Arc;

use crate::db::Store;
use crate::types::*;

/// Builds symbol-level edges from resolved references.
///
/// This is the second half of the resolution pipeline:
/// 1. `ReferenceResolver` resolves references → `Vec<(ReferenceUse, ResolvedTarget)>`
/// 2. `GraphBuilder` converts those resolved targets → `Vec<RawEdge>`
pub struct GraphBuilder {
    store: Arc<Store>,
}

impl GraphBuilder {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Build all symbol-level edges from a batch of resolved references.
    ///
    /// This is the primary entry point: takes the output of
    /// `ReferenceResolver::resolve_all()` and produces edges.
    ///
    /// Returns `GraphBuilderStats` with edge count and any non-fatal warnings.
    pub fn build_all(
        &self,
        resolved: &[(ReferenceUse, ResolvedTarget)],
    ) -> GraphBuilderStats {
        let mut edges = Vec::new();
        let mut warnings = Vec::new();

        for (reference, target) in resolved {
            match self.create_edges_for_reference(reference, target) {
                Ok(mut new_edges) => edges.append(&mut new_edges),
                Err(e) => warnings.push(format!(
                    "failed to create edges for reference {:?}: {}",
                    reference.id, e
                )),
            }
        }

        let edge_count = edges.len();

        // Write edges to store
        if !edges.is_empty() {
            if let Err(e) = self.store.batch_insert_edges(&edges) {
                warnings.push(format!("batch edge insert failed ({} edges): {}", edge_count, e));
            }
        }

        GraphBuilderStats {
            edges_created: edge_count,
            warnings,
        }
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
    /// - `Contains` from container → child when the target has a container
    fn create_edges_for_reference(
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
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
                    EdgeKind::Calls
                }
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

/// Statistics from a GraphBuilder run.
#[derive(Debug, Clone, Default)]
pub struct GraphBuilderStats {
    pub edges_created: usize,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::extract_file;
    use crate::extraction::languages::typescript::TypeScriptAdapter;
    use crate::resolution::ReferenceResolver;
    use std::path::PathBuf;

    /// Test that GraphBuilder produces edges from resolved references.
    #[test]
    fn test_graph_builder_basic() {
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let lib_facts = extract_file(
            &TypeScriptAdapter,
            lib_id,
            &PathBuf::from("lib.ts"),
            lib_src,
            "abc",
        )
        .expect("lib.ts extraction failed");

        let main_src = r#"import { greet } from './lib';

function main() {
    const msg = greet("World");
}
main();
"#;
        let main_id = FileId::generate("main.ts");
        let main_facts = extract_file(
            &TypeScriptAdapter,
            main_id,
            &PathBuf::from("main.ts"),
            main_src,
            "abc",
        )
        .expect("main.ts extraction failed");

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store.insert_file_facts(&main_facts).expect("insert main.ts");

        // Resolve
        let resolver = ReferenceResolver::new(store.clone());
        let (resolved, _res_stats) = resolver
            .resolve_all()
            .expect("resolution failed");

        // Build edges
        let builder = GraphBuilder::new(store.clone());
        let stats = builder.build_all(&resolved);

        assert!(
            stats.edges_created > 0,
            "expected at least 1 edge, got {}",
            stats.edges_created
        );
    }
}
