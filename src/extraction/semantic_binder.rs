//! Semantic binder — extraction-time source ownership and scope binding.
//!
//! This module wraps [`SymbolRegistry`] and adds scope-binding capability.
//! The extraction pipeline uses `SemanticBinder` to:
//!
//! 1. **Bind source** — resolve `source_symbol` for each reference via the
//!    innermost owning symbol (delegated to `SymbolRegistry`).
//! 2. **Bind scope** — fill `scope_id` for each reference by finding the
//!    innermost scope that contains the reference's byte range.
//! 3. **Bind edge sources** — rewrite/drop raw edges whose source cannot be
//!    tied to an extracted definition (delegated to `SymbolRegistry`).
//!
//! Language adapters should **not** fill `source_symbol` or `scope_id` in
//! `normalize_reference()`; the binder is the single authority for these
//! fields.

use crate::types::ids::{FileId, ScopeId};
use crate::types::{RawEdge, ReferenceUse, ScopeDef, SymbolDef};

use super::symbol_registry::SymbolRegistry;

/// Extraction-time semantic binder.
///
/// Wraps a [`SymbolRegistry`] and adds scope-binding on top of source
/// ownership resolution.  Construct with [`SemanticBinder::new()`] and call
/// [`SemanticBinder::bind_all()`] in the extraction pipeline.
#[derive(Debug, Clone)]
pub struct SemanticBinder {
    registry: SymbolRegistry,
    scopes: Vec<ScopeDef>,
}

impl SemanticBinder {
    /// Build a binder from extracted definitions and scope tree.
    ///
    /// This internally constructs a `SymbolRegistry` for source-ownership
    /// resolution and retains the scope list for scope-binding.
    pub fn new(symbols: &[SymbolDef], scopes: &[ScopeDef]) -> Self {
        Self {
            registry: SymbolRegistry::new(symbols, scopes),
            scopes: scopes.to_vec(),
        }
    }

    /// Bind source symbols for all references (delegates to SymbolRegistry).
    ///
    /// Rewrites `reference.source_symbol` to the innermost owning symbol.
    /// Regenerates `ReferenceId` when the source changes.
    pub fn bind_source(&self, file_id: FileId, references: &mut [ReferenceUse]) {
        self.registry.resolve_reference_sources(file_id, references);
    }

    /// Bind scope IDs for all references.
    ///
    /// Fills `reference.scope_id` with the innermost scope containing the
    /// reference's byte range.  If no scope contains the reference, sets
    /// `scope_id` to `None`.
    ///
    /// **Note**: `scope_id` does **not** participate in `ReferenceId`
    /// generation, so no ID regeneration is needed here.
    pub fn bind_scope(&self, references: &mut [ReferenceUse]) {
        for reference in references {
            reference.scope_id = self.innermost_scope(reference.range);
        }
    }

    /// Bind source symbols for raw edges (delegates to SymbolRegistry).
    ///
    /// Rewrites edge sources and drops edges whose source cannot be tied to an
    /// extracted definition.
    pub fn bind_edge_sources(&self, edges: &mut Vec<RawEdge>) {
        self.registry.resolve_edge_sources(edges);
    }

    /// Convenience method: bind source, scope, and edge sources in one call.
    ///
    /// This is the typical entry point for the extraction pipeline.
    pub fn bind_all(
        &self,
        file_id: FileId,
        references: &mut [ReferenceUse],
        edges: &mut Vec<RawEdge>,
    ) {
        self.bind_source(file_id, references);
        self.bind_scope(references);
        self.bind_edge_sources(edges);
    }

    /// Find the innermost scope containing the given range.
    fn innermost_scope(&self, range: crate::types::TextRange) -> Option<ScopeId> {
        self.scopes
            .iter()
            .filter(|scope| contains_range(scope.range, range))
            .min_by_key(|scope| scope.range.byte_len())
            .map(|scope| scope.id)
    }

    /// Delegate: check if a symbol is known.
    pub fn contains_symbol(&self, id: &crate::types::ids::SymbolId) -> bool {
        self.registry.contains_symbol(id)
    }

    /// Delegate: resolve source for an arbitrary range.
    pub fn source_for_range(&self, range: crate::types::TextRange) -> Option<crate::types::ids::SymbolId> {
        self.registry.source_for_range(range)
    }
}

fn contains_range(outer: crate::types::TextRange, inner: crate::types::TextRange) -> bool {
    outer.start_byte <= inner.start_byte && outer.end_byte >= inner.end_byte
}

#[cfg(test)]
mod tests {
    use crate::types::ids::{FileId, ScopeId, SymbolId};
    use crate::types::{
        Language, ReferenceKind, ReferenceUse, ScopeDef, ScopeKind, SymbolDef, SymbolKind,
        TextRange,
    };
    use crate::extraction::SemanticBinder;

    fn make_file_id() -> FileId {
        FileId::generate("test.ts")
    }

    fn make_symbol(name: &str, kind: SymbolKind, range: TextRange, scope_id: Option<ScopeId>) -> SymbolDef {
        SymbolDef {
            id: SymbolId::generate(&make_file_id(), "typescript", name, kind.as_str(), None),
            kind,
            name: name.to_string(),
            qualified_name: format!("mod::{name}"),
            symbol_path: vec![name.to_string()],
            file_id: make_file_id(),
            language: Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id,
            package_name: None,
            namespace_path: vec![],
        }
    }

    fn make_scope(kind: ScopeKind, name: &str, range: TextRange, parent_id: Option<ScopeId>) -> ScopeDef {
        let file_id = make_file_id();
        ScopeDef {
            id: ScopeId::generate(&file_id, parent_id.as_ref(), kind.as_str(), range.start_byte),
            file_id,
            kind,
            name: name.to_string(),
            scope_path: name.to_string(),
            range,
            parent_id,
        }
    }

    fn make_reference(text: &str, kind: ReferenceKind, range: TextRange) -> ReferenceUse {
        let file_id = make_file_id();
        ReferenceUse {
            id: crate::types::ids::ReferenceId::generate(&file_id, None, range.start_byte, range.end_byte, text, kind),
            file_id,
            source_symbol: None,
            scope_id: None,
            kind,
            text: text.to_string(),
            name: text.to_string(),
            receiver: None,
            arity: None,
            range,
            resolved: None,
        }
    }

    #[test]
    fn test_bind_scope_fills_scope_id() {
        let file_range = TextRange { start_byte: 0, end_byte: 100, start_line: 1, start_column: 0, end_line: 10, end_column: 0 };
        let func_range = TextRange { start_byte: 10, end_byte: 80, start_line: 2, start_column: 0, end_line: 8, end_column: 0 };
        let ref_range = TextRange { start_byte: 30, end_byte: 35, start_line: 3, start_column: 5, end_line: 3, end_column: 10 };

        let file_scope = make_scope(ScopeKind::File, "test.ts", file_range, None);
        let func_scope = make_scope(ScopeKind::Function, "foo", func_range, Some(file_scope.id));
        let func_scope_id = func_scope.id;

        let func_sym = make_symbol("foo", SymbolKind::Function, func_range, Some(func_scope_id));

        let mut refs = vec![make_reference("bar", ReferenceKind::Usage, ref_range)];

        let binder = SemanticBinder::new(&[func_sym], &[file_scope, func_scope]);
        binder.bind_scope(&mut refs);

        assert_eq!(refs[0].scope_id, Some(func_scope_id));
    }

    #[test]
    fn test_bind_source_and_scope_together() {
        let file_range = TextRange { start_byte: 0, end_byte: 100, start_line: 1, start_column: 0, end_line: 10, end_column: 0 };
        let func_range = TextRange { start_byte: 10, end_byte: 80, start_line: 2, start_column: 0, end_line: 8, end_column: 0 };
        let ref_range = TextRange { start_byte: 30, end_byte: 35, start_line: 3, start_column: 5, end_line: 3, end_column: 10 };

        let file_scope = make_scope(ScopeKind::File, "test.ts", file_range, None);
        let func_scope = make_scope(ScopeKind::Function, "foo", func_range, Some(file_scope.id));
        let func_scope_id = func_scope.id;

        let func_sym = make_symbol("foo", SymbolKind::Function, func_range, Some(func_scope_id));

        let mut refs = vec![make_reference("bar", ReferenceKind::Usage, ref_range)];
        let mut edges = vec![];

        let binder = SemanticBinder::new(&[func_sym], &[file_scope, func_scope]);
        binder.bind_all(make_file_id(), &mut refs, &mut edges);

        assert!(refs[0].source_symbol.is_some());
        assert_eq!(refs[0].scope_id, Some(func_scope_id));
    }
}
