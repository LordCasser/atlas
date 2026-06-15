//! Symbol registry for extraction-time source ownership.
//!
//! This module is the single source of truth for mapping references/dataflow
//! captures back to the symbol that contains them.  Language adapters set
//! `source_symbol: None` in `normalize_reference()`; the extraction pipeline
//! validates and rewrites those IDs through this registry so edges/callsites
//! never point at symbols that were not actually produced by the definitions
//! query.

use std::collections::{HashMap, HashSet};

use types::ids::{EdgeId, FileId, ReferenceId, ScopeId, SymbolId};
use types::{RawEdge, ReferenceUse, ScopeDef, SymbolDef, SymbolKind, TextRange};

/// Definitions-derived symbol table used to resolve source symbols for
/// references and dataflow edges.  It is intentionally built only from the
/// symbols/scopes that extraction has actually produced.
#[derive(Debug, Clone)]
pub struct SymbolRegistry {
    known_symbols: HashSet<SymbolId>,
    scopes: Vec<ScopeDef>,
    scope_parents: HashMap<ScopeId, ScopeId>,
    owner_by_scope: HashMap<ScopeId, SymbolId>,
}

impl SymbolRegistry {
    /// Build a registry from normalized definitions and the reconstructed scope
    /// tree.  No IDs are generated here; all owners come from `symbols`.
    pub fn new(symbols: &[SymbolDef], scopes: &[ScopeDef]) -> Self {
        let known_symbols = symbols.iter().map(|s| s.id).collect();
        let scope_parents = scopes
            .iter()
            .filter_map(|s| s.parent_id.map(|pid| (s.id, pid)))
            .collect::<HashMap<_, _>>();

        // Map each executable/type scope to the best symbol that owns it.  More
        // specific callable symbols beat class/namespace owners when several
        // symbols land in the same scope.
        let mut owner_by_scope: HashMap<ScopeId, (u8, u32, SymbolId)> = HashMap::new();
        for sym in symbols {
            let Some(scope_id) = sym.scope_id else {
                continue;
            };
            let Some(priority) = source_symbol_priority(sym.kind) else {
                continue;
            };
            let span = sym.range.byte_len();
            match owner_by_scope.get(&scope_id) {
                Some((old_priority, old_span, _))
                    if *old_priority > priority
                        || (*old_priority == priority && *old_span <= span) => {}
                _ => {
                    owner_by_scope.insert(scope_id, (priority, span, sym.id));
                }
            }
        }

        Self {
            known_symbols,
            scopes: scopes.to_vec(),
            scope_parents,
            owner_by_scope: owner_by_scope
                .into_iter()
                .map(|(scope, (_, _, symbol))| (scope, symbol))
                .collect(),
        }
    }

    /// Whether a SymbolId is one of the definitions extracted for this file.
    pub fn contains_symbol(&self, id: &SymbolId) -> bool {
        self.known_symbols.contains(id)
    }

    /// Resolve the source/owner symbol for an arbitrary source range.
    pub fn source_for_range(&self, range: TextRange) -> Option<SymbolId> {
        let mut current = crate::languages::shared::innermost_scope(&self.scopes, range);
        while let Some(scope_id) = current {
            if let Some(owner) = self.owner_by_scope.get(&scope_id) {
                return Some(*owner);
            }
            current = self.scope_parents.get(&scope_id).copied();
        }
        None
    }

    /// Rewrite reference source symbols through the registry.  If a capture is
    /// outside any known owner, its source becomes `None`.  Reference IDs are
    /// regenerated whenever the source changes because source participates in
    /// `ReferenceId` generation.
    pub fn resolve_reference_sources(&self, file_id: FileId, references: &mut [ReferenceUse]) {
        for reference in references {
            let resolved_source = self.source_for_range(reference.range);
            if reference.source_symbol != resolved_source {
                reference.source_symbol = resolved_source;
                reference.id = ReferenceId::generate(
                    &file_id,
                    reference.source_symbol.as_ref(),
                    reference.range.start_byte,
                    reference.range.end_byte,
                    &reference.text,
                    reference.kind,
                );
            }
        }
    }

    /// Rewrite raw edge source symbols through the registry and drop edges whose
    /// source cannot be tied to an extracted definition.  Edge IDs are
    /// regenerated if the source changes.
    pub fn resolve_edge_sources(&self, edges: &mut Vec<RawEdge>) {
        edges.retain_mut(|edge| {
            let resolved_source = edge
                .location
                .and_then(|range| self.source_for_range(range))
                .or_else(|| self.contains_symbol(&edge.source).then_some(edge.source));

            let Some(source) = resolved_source else {
                return false;
            };
            if !self.contains_symbol(&source) {
                return false;
            }
            if edge.source != source {
                edge.source = source;
                edge.id = regenerate_edge_id(edge);
            }
            true
        });
    }
}

fn source_symbol_priority(kind: SymbolKind) -> Option<u8> {
    match kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => Some(100),
        SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Interface
        | SymbolKind::Trait
        | SymbolKind::Enum => Some(80),
        SymbolKind::Namespace | SymbolKind::Module | SymbolKind::Package => Some(60),
        _ => None,
    }
}

fn regenerate_edge_id(edge: &RawEdge) -> EdgeId {
    EdgeId::generate(
        &edge.source,
        &edge.target,
        edge.kind.as_str(),
        edge.ref_id.as_ref(),
        edge.provenance.as_str(),
    )
}

/// Defensive postcondition check used by tests and future validation hooks.
pub fn all_edge_sources_known(edges: &[RawEdge], symbols: &[SymbolDef]) -> bool {
    let known = symbols.iter().map(|s| s.id).collect::<HashSet<_>>();
    edges.iter().all(|edge| known.contains(&edge.source))
}

/// Defensive postcondition check used by tests and future validation hooks.
pub fn all_reference_sources_known(references: &[ReferenceUse], symbols: &[SymbolDef]) -> bool {
    let known = symbols.iter().map(|s| s.id).collect::<HashSet<_>>();
    references.iter().all(|reference| {
        reference
            .source_symbol
            .map(|id| known.contains(&id))
            .unwrap_or(true)
    })
}
