//! Lexical binder — per-file lexical binding extraction.
//!
//! The LexicalBinder extracts binding definitions (parameters, locals, import aliases,
//! catch variables, fields) from tree-sitter ASTs via the adapter's `lexical_query()`.
//!
//! # Architecture
//!
//! - Runs the adapter's `lexical_query()` to find binding definition sites.
//! - Resolves scope containment for each binding (innermost enclosing scope).
//! - Creates one [`BindingUse`] per binding definition (declaration-as-use for dataflow).
//! - Does NOT scan standalone identifier references or resolve shadowing.
//! - Does NOT resolve `function_id` or `symbol_id` — those are filled by post-extraction
//!   steps (scope tree, SemanticBinder).
//!
//! # Current limitations (not yet implemented)
//!
//! - Identifier-use scanning (every variable reference as a BindingUse).
//! - Shadowing resolution across scope chains.
//! - Per-function grouping (function_id always None).
//!
//! # Invariants
//!
//! - Every `BindingDef` has a non-empty `scope_id` (resolved here) and `name`.
//! - Every `BindingUse` has `file_id`, `scope_id`, `name`, and `range`.
//! - `BindingUse::binding_id` may be `None` if unresolved (e.g. external reference).
//! - `function_id` is always `None` at this stage; downstream consumers should fill it.

use crate::extraction_ctx::ExtractionCtx;
use crate::frontend::{Capture, LexicalBindingSpec};
use atlas_types::bindings::{BindingDef, BindingUse};
use atlas_types::ids::{BindingId, BindingUseId, ScopeId};
use atlas_types::structs::TextRange;
use atlas_types::{ScopeDef, SymbolDef};

use super::query_helpers::collect_captures;

/// Result of lexical binding extraction.
#[derive(Debug, Clone)]
pub struct LexicalBindingResult {
    /// All binding definitions found in the file.
    pub bindings: Vec<BindingDef>,
    /// All binding use sites found in the file.
    pub uses: Vec<BindingUse>,
}

/// Extracts lexical bindings (parameters, locals, import aliases, etc.)
/// from tree-sitter AST captures.
///
/// # Usage
///
/// ```ignore
/// let result = LexicalBinder::extract(
///     &adapter, &ts_lang, &root_node, source, file_id, file_path,
///     &scopes, &symbols,
/// )?;
/// ```
pub struct LexicalBinder;

impl LexicalBinder {
    /// Extract lexical binding definitions and declaration-as-use sites.
    ///
    /// Runs the adapter's `lexical_query()`, normalizes captures into
    /// `BindingDef` structs, resolves scope containment, and creates
    /// one `BindingUse` per binding definition.
    pub(crate) fn extract(
        lexical_spec: &dyn LexicalBindingSpec,
        ctx: &ExtractionCtx<'_>,
        scopes: &[ScopeDef],
        _symbols: &[SymbolDef],
    ) -> anyhow::Result<LexicalBindingResult> {
        let query_src = lexical_spec.lexical_query();
        if query_src.trim().is_empty() {
            return Ok(LexicalBindingResult {
                bindings: vec![],
                uses: vec![],
            });
        }

        // Collect raw captures
        let captures = collect_captures(
            ctx.ts_lang,
            query_src,
            ctx.root,
            ctx.source_bytes(),
            "lexical",
        )
        .map_err(|failure| {
            use crate::error::ExtractionFailure;
            let filled = ExtractionFailure {
                file_path: ctx.file_path.to_string_lossy().to_string(),
                language: ctx.language,
                ..failure
            };
            anyhow::Error::new(filled)
        })?;

        // Normalize each capture into a BindingDef
        let mut bindings: Vec<BindingDef> = Vec::new();
        let nctx = ctx.normalize_ctx();
        for (name, node) in captures {
            let capture = Capture { name, node };
            match lexical_spec.normalize(nctx, capture) {
                Some(binding) => bindings.push(binding),
                None => {
                    // Non-fatal: captures that don't produce a binding are normal
                    // (e.g., optional_parameter without an identifier)
                }
            }
        }

        // Resolve scope containment for each binding:
        // Replace placeholder scope_id with the actual innermost scope.
        for binding in &mut bindings {
            binding.scope_id = innermost_scope(scopes, binding.range).unwrap_or(binding.scope_id);
            // Re-generate BindingId now that scope_id is correct
            binding.id = BindingId::generate(
                &ctx.file_id,
                &binding.scope_id,
                binding.kind.as_str(),
                &binding.name,
                binding.range.start_byte,
            );
        }

        // For each binding definition, also create a BindingUse at the same point
        // (binding definitions are also uses — e.g., a parameter declaration is also
        // a use site for the purpose of dataflow).
        let mut uses: Vec<BindingUse> = Vec::new();
        for binding in &bindings {
            let use_id = BindingUseId::generate(
                &ctx.file_id,
                Some(&binding.id),
                None::<&atlas_types::ids::ReferenceId>,
                &binding.name,
                binding.range.start_byte,
            );
            uses.push(BindingUse {
                id: use_id,
                file_id: ctx.file_id,
                scope_id: binding.scope_id,
                binding_id: Some(binding.id),
                reference_id: None,
                name: binding.name.clone(),
                range: binding.range,
            });
        }

        Ok(LexicalBindingResult { bindings, uses })
    }
}

/// Find the innermost scope that fully contains the given byte range.
fn innermost_scope(scopes: &[ScopeDef], range: TextRange) -> Option<ScopeId> {
    scopes
        .iter()
        .filter(|scope| {
            scope.range.start_byte <= range.start_byte && scope.range.end_byte >= range.end_byte
        })
        .min_by_key(|scope| scope.range.byte_len())
        .map(|scope| scope.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_types::Language;
    use atlas_types::enums::BindingKind;
    use atlas_types::ids::FileId;
    use atlas_types::{ScopeKind, TextRange};
    use std::path::PathBuf;

    #[test]
    fn test_innermost_scope_finds_correct_scope() {
        let file_id = FileId::generate("test.ts");
        let outer = ScopeDef {
            id: ScopeId::generate(&file_id, None, "file", 0),
            file_id,
            kind: ScopeKind::File,
            name: "file".into(),
            scope_path: "file".into(),
            parent_id: None,
            range: TextRange {
                start_byte: 0,
                end_byte: 100,
                start_line: 1,
                start_column: 0,
                end_line: 10,
                end_column: 0,
            },
        };
        let inner = ScopeDef {
            id: ScopeId::generate(&file_id, Some(&outer.id), "function", 10),
            file_id,
            kind: ScopeKind::Function,
            name: "func".into(),
            scope_path: "func".into(),
            parent_id: Some(outer.id),
            range: TextRange {
                start_byte: 10,
                end_byte: 80,
                start_line: 2,
                start_column: 0,
                end_line: 8,
                end_column: 0,
            },
        };

        let scopes = vec![outer, inner.clone()];
        let target = TextRange {
            start_byte: 30,
            end_byte: 35,
            start_line: 3,
            start_column: 5,
            end_line: 3,
            end_column: 10,
        };
        let result = innermost_scope(&scopes, target);
        assert_eq!(result, Some(inner.id));
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_lexical_binder_extracts_ts_bindings() {
        use crate::frontend::ParserSpec;
        use crate::languages::typescript::TypeScriptFrontendSpec;
        use tree_sitter::Parser;

        let source =
            "function handler(req: any) {\n  const name = req.body.name;\n  return name;\n}";
        let file_id = FileId::generate("test.ts");
        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let lexical_spec: &dyn LexicalBindingSpec = &spec;

        let mut parser = Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let root = tree.root_node();

        // Need scopes for scope resolution
        let scopes: Vec<ScopeDef> = vec![];
        let symbols: Vec<SymbolDef> = vec![];
        let file_path = PathBuf::from("test.ts");

        let ctx = ExtractionCtx {
            ts_lang: &ts_lang,
            root,
            source,
            file_id,
            file_path: &file_path,
            language: Language::TypeScript,
        };

        let result = LexicalBinder::extract(lexical_spec, &ctx, &scopes, &symbols).unwrap();

        assert!(!result.bindings.is_empty(), "Should have lexical bindings");
        // Should have at least 'req' (parameter) and 'name' (local)
        let param_names: Vec<_> = result
            .bindings
            .iter()
            .filter(|b| b.kind == BindingKind::Parameter)
            .map(|b| b.name.as_str())
            .collect();
        let local_names: Vec<_> = result
            .bindings
            .iter()
            .filter(|b| b.kind == BindingKind::Local)
            .map(|b| b.name.as_str())
            .collect();

        assert!(
            param_names.contains(&"req"),
            "Expected parameter 'req', got: {param_names:?}"
        );
        assert!(
            local_names.contains(&"name"),
            "Expected local 'name', got: {local_names:?}"
        );
    }
}
