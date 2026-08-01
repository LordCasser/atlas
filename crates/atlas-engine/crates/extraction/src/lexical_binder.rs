//! Lexical binder — per-file lexical binding extraction.
//!
//! The LexicalBinder extracts binding definitions (parameters, locals, import aliases,
//! catch variables, fields) from tree-sitter ASTs via the adapter's `lexical_query()`.
//!
//! # Architecture
//!
//! - Runs the adapter's `lexical_query()` to find binding definition sites.
//! - Resolves scope containment for each binding (innermost enclosing scope).
//! - Optionally coalesces repeated same-scope sites when the language uses
//!   namespace identity rather than declaration identity.
//! - Creates one [`BindingUse`] per declaration/write site for dataflow.
//! - Does NOT scan standalone identifier references; that happens downstream.
//! - Does NOT resolve `function_id` or `symbol_id` — those are filled by post-extraction
//!   steps (scope tree, SemanticBinder).
//!
//! # Current limitations (not yet implemented)
//!
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
use std::collections::HashMap;
use types::bindings::{BindingDef, BindingUse};
use types::ids::{BindingId, BindingUseId};
use types::{ScopeDef, SymbolDef};

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
    /// one `BindingUse` per captured binding site.
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
            None,
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
            binding.scope_id = crate::languages::shared::innermost_scope(scopes, binding.range)
                .unwrap_or(binding.scope_id);
            // Re-generate BindingId now that scope_id is correct
            binding.id = BindingId::generate(
                &ctx.file_id,
                &binding.scope_id,
                binding.kind.as_str(),
                &binding.name,
                binding.range.start_byte,
            );
        }

        // Some languages have namespace identity rather than declaration
        // identity. Preserve every declaration as a use/write event, but keep
        // one canonical BindingDef for each (scope, name).
        bindings.sort_by_key(|binding| binding.range.start_byte);
        let mut canonical_ids = HashMap::new();
        if lexical_spec.coalesce_same_scope_bindings() {
            for binding in &bindings {
                canonical_ids
                    .entry((binding.scope_id, binding.name.clone()))
                    .or_insert(binding.id);
            }
        }

        // For each raw binding site, create a BindingUse at the same point.
        let mut uses: Vec<BindingUse> = Vec::new();
        for binding in &bindings {
            let binding_id = canonical_ids
                .get(&(binding.scope_id, binding.name.clone()))
                .copied()
                .unwrap_or(binding.id);
            let use_id = BindingUseId::generate(
                &ctx.file_id,
                Some(&binding_id),
                None::<&types::ids::ReferenceId>,
                &binding.name,
                binding.range.start_byte,
            );
            uses.push(BindingUse {
                id: use_id,
                file_id: ctx.file_id,
                scope_id: binding.scope_id,
                binding_id: Some(binding_id),
                reference_id: None,
                name: binding.name.clone(),
                range: binding.range,
            });
        }

        if lexical_spec.coalesce_same_scope_bindings() {
            bindings.retain(|binding| {
                canonical_ids
                    .get(&(binding.scope_id, binding.name.clone()))
                    .is_some_and(|id| *id == binding.id)
            });
        }

        Ok(LexicalBindingResult { bindings, uses })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use types::Language;
    use types::enums::BindingKind;
    use types::ids::{FileId, ScopeId};
    use types::{ScopeKind, TextRange};

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
        let result = crate::languages::shared::innermost_scope(&scopes, target);
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
