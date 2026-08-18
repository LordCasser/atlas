//! LanguageFrontend — slot-based language frontend.
//!
//! ## Design
//!
//! `LanguageFrontend` is a struct with typed slot fields. Each slot is a
//! trait object (`Box<dyn SomeSpec>`) with a `capability()` method that
//! returns `FeatureSupport` instead of silently returning empty data.
//!
//! Slots use a unified `NormalizeCtx`/`Capture` API for all normalizations,
//! passing file context and raw tree-sitter captures without requiring
//! per-method parameter lists.
//!
//! Unsupported slots are filled with typed `Unsupported*Spec` structs that
//! return `FeatureSupport::Unsupported(reason)`.
//!
//! ## Construction
//!
//! Use the `*_frontend()` factory for each language (e.g.
//! `typescript_frontend()`). These construct slots directly via
//! `LanguageFrontend::from_parts()`.

use crate::callsite_spec::CallsiteExtractorSpec;
use crate::dataflow_builder::NodePosKey;
use crate::extraction_ctx::ExtractionCtx;
use types::bindings::BindingDef;
use types::capability::{FeatureMatrix, FeatureSupport, LanguageCapabilityProfile};
use types::dataflow::{DataFlowEdge, DataNode};
use types::enums::{Language, ScopeKind};
use types::ids::{DataNodeId, FileId, ScopeId};
use types::structs::{ImportDef, ReferenceUse, ScopeDef, SymbolDef};

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Normalize context / capture (replaces multi-param normalize signatures)
// ---------------------------------------------------------------------------

/// Context available during capture normalization.
///
/// Bundles everything a normalize method needs — file identity, source text,
/// and language — into a single value that is shared across all captures for
/// a given file extraction.
#[derive(Clone, Copy)]
pub struct NormalizeCtx<'a> {
    /// The language being extracted.
    pub language: Language,
    /// File identifier for the source file.
    pub file_id: FileId,
    /// Path of the source file on disk.
    pub file_path: &'a Path,
    /// Raw source text of the file.
    pub source: &'a str,
}

/// A single tree-sitter query capture pending normalization.
pub struct Capture<'a> {
    /// Capture name from the tree-sitter query (e.g. `"function"`, `"call"`).
    pub name: String,
    /// The captured syntax node.
    pub node: tree_sitter::Node<'a>,
}

// ---------------------------------------------------------------------------
// Spec traits
// ---------------------------------------------------------------------------

/// Parser spec: language identity + tree-sitter grammar.
pub trait ParserSpec: Send + Sync {
    /// The Language variant this frontend handles.
    fn language(&self) -> Language;
    /// Tree-sitter Language grammar for parsing.
    fn tree_sitter_language(&self) -> tree_sitter::Language;
    /// Return the source presented to tree-sitter.
    ///
    /// Language frontends may perform byte-length-preserving normalization for
    /// syntax that a fallback grammar cannot recognize. Ranges in the parsed
    /// tree must remain valid against the original source.
    fn parser_source<'a>(&self, source: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(source)
    }
    /// Optionally provide a byte-stable recovery source for declaration and
    /// scope extraction after the primary parse has completed.
    ///
    /// References, callsites, lexical uses, dataflow, and CFG always consume
    /// the primary tree. This hook exists for fallback grammars whose error
    /// recovery can lose declaration boundaries while still preserving useful
    /// expression facts in the primary tree.
    fn declaration_recovery_source(
        &self,
        _parser_source: &str,
        _primary_root: tree_sitter::Node<'_>,
    ) -> Option<String> {
        None
    }
    /// Feature support for parsing.
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

/// Symbol extraction spec: tree-sitter query + normalization.
pub trait SymbolExtractorSpec: Send + Sync {
    /// S-expression query for symbol definitions.
    fn definition_query(&self) -> &str;
    /// S-expression query for manifest-level (top-level only) symbol definitions.
    ///
    /// MUST return a query that captures **only top-level** (file-scope) declarations.
    /// Every language MUST override this (no default) to enforce the Manifest layer
    /// contract (see ExtractionMode::Manifest, architecture.md, and testing.md).
    /// The query is typically a wrapper such as (translation_unit ...) or (program ...)
    /// around top-level definition patterns only; nested symbols inside bodies must
    /// not be captured. All 14 current languages provide a dedicated
    /// queries/<lang>/manifest.scm and override.
    fn manifest_query(&self) -> &str;
    /// Feature support for symbol extraction.
    fn capability(&self) -> FeatureSupport;
    /// Normalize a definition capture into a [`SymbolDef`], or `None`
    /// if the capture isn't a valid definition.
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef>;
}

/// Reference extraction spec: tree-sitter query + normalization.
pub trait ReferenceExtractorSpec: Send + Sync {
    /// S-expression query for reference uses.
    fn reference_query(&self) -> &str;
    /// Feature support for reference extraction.
    fn capability(&self) -> FeatureSupport;
    /// Normalize a reference-use capture into a [`ReferenceUse`], or `None`.
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse>;
}

/// Import extraction spec: tree-sitter query + normalization.
pub trait ImportExtractorSpec: Send + Sync {
    /// S-expression query for import statements.
    fn import_query(&self) -> &str;
    /// Feature support for import extraction.
    fn capability(&self) -> FeatureSupport;
    /// Normalize an import capture into an [`ImportDef`], or `None`.
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef>;
}

/// Scope extraction spec: tree-sitter query + normalization.
pub trait ScopeExtractorSpec: Send + Sync {
    /// S-expression query for scopes.
    fn scope_query(&self) -> &str;
    /// Feature support for scope extraction.
    fn capability(&self) -> FeatureSupport;
    /// Normalize a scope capture into a [`ScopeDef`], or `None`.
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef>;
}

/// Lexical binding extraction spec.
pub trait LexicalBindingSpec: Send + Sync {
    /// S-expression query for lexical bindings.
    fn lexical_query(&self) -> &str;
    /// Feature support for lexical binding extraction.
    fn capability(&self) -> FeatureSupport;
    /// Normalize a lexical capture into a [`BindingDef`], or `None`.
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef>;

    /// Query used to collect lexical identifier use sites after declarations
    /// have been extracted. Most grammars call this node `identifier`.
    fn binding_use_query(&self) -> &str {
        "(identifier) @binding.use"
    }

    /// Language-specific syntax filter for lexical use captures.
    fn is_binding_use(&self, _node: tree_sitter::Node<'_>) -> bool {
        true
    }

    /// Normalize an identifier captured by [`Self::binding_use_query`] into
    /// the same namespace used by [`Self::normalize`].
    fn normalize_binding_use_name(&self, raw: &str) -> String {
        raw.to_string()
    }

    /// Whether repeated declarations of a name in one scope share one binding.
    ///
    /// Most supported languages use declaration identity. Languages such as
    /// Python instead use one local namespace per function/module/class, where
    /// assignment sites are writes to an existing name rather than shadowing
    /// declarations.
    fn coalesce_same_scope_bindings(&self) -> bool {
        false
    }

    /// Whether a structural scope also introduces a lexical namespace.
    ///
    /// Scope facts describe both source structure and name lookup boundaries.
    /// Those concepts are not identical in every language: for example, Ruby
    /// conditionals and loops are structural scopes but share their enclosing
    /// local-variable namespace.
    fn is_lexical_scope(&self, _kind: ScopeKind) -> bool {
        true
    }

    /// Whether names unresolved in `scope` may continue into its parent scope.
    ///
    /// Most languages use lexical capture across nested scopes. Languages with
    /// explicit callable capture rules can stop lookup at the callable scope
    /// and represent allowed captures as bindings inside that scope.
    fn inherits_bindings_from_parent(&self, _scope: &ScopeDef) -> bool {
        true
    }

    /// Select the namespace that owns a binding after initial containment.
    ///
    /// `preceding_bindings` contains only source-earlier bindings whose scopes
    /// have already been finalized. Most languages retain the innermost scope;
    /// source-ordered namespace languages may reuse an existing ancestor.
    fn binding_scope(
        &self,
        binding: &BindingDef,
        _lexical_scopes: &[ScopeDef],
        _preceding_bindings: &[BindingDef],
    ) -> ScopeId {
        binding.scope_id
    }
}

/// Dataflow extraction spec.
pub(crate) trait DataflowSpec: Send + Sync {
    /// S-expression query for dataflow builder.
    fn dataflow_builder_query(&self) -> &str;
    /// Feature support for dataflow extraction.
    fn capability(&self) -> FeatureSupport;
    /// Normalize a dataflow capture into a ([`DataNode`], [`DataFlowEdge`]), or
    /// `(None, None)`.
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>);

    /// Build language-specific dataflow edges after normalization.
    ///
    /// Called by [`DataFlowBuilder`] after all captures are normalized and
    /// the default (shared) edges are built.  Each language adapter can
    /// override this to add edges for language-specific AST patterns such as
    /// destructuring, tuple unpacking, match bindings, multi‑return, etc.
    ///
    /// The default implementation is a no‑op — all edge building for standard
    /// patterns (assignment, field access, containment, return) happens in the
    /// shared builder.  Language adapters that need custom AST walking can use
    /// `ctx.root` to walk the tree and `pos_map` to look up DataNodeIds by
    /// byte range and kind.
    fn build_language_edges(
        &self,
        _ctx: &ExtractionCtx<'_>,
        _pos_map: &HashMap<NodePosKey, DataNodeId>,
        _nodes: &[DataNode],
        _bindings: &[BindingDef],
        _scopes: &[ScopeDef],
        _edges: &mut Vec<DataFlowEdge>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FrontendParts
// ---------------------------------------------------------------------------

/// Named slot bundle for constructing a [`LanguageFrontend`] directly.
///
/// Used by per-language `*_frontend()` factories to pass all slot
/// implementations in a single struct, avoiding long positional argument lists.
pub struct FrontendParts {
    pub parser: Box<dyn ParserSpec>,
    pub symbols: Box<dyn SymbolExtractorSpec>,
    pub references: Box<dyn ReferenceExtractorSpec>,
    pub imports: Box<dyn ImportExtractorSpec>,
    pub scopes: Box<dyn ScopeExtractorSpec>,
    pub callsites: Box<dyn CallsiteExtractorSpec>,
    pub lexical: Box<dyn LexicalBindingSpec>,
    pub(crate) dataflow: Box<dyn DataflowSpec>,
    pub capability: LanguageCapabilityProfile,
}

// ---------------------------------------------------------------------------
// LanguageFrontend
// ---------------------------------------------------------------------------

/// Slot-based language frontend.
///
/// Each slot is a trait object with a `capability()` method, enabling
/// type-safe feature queries.
///
/// ## Construction
///
/// Use [`FrontendParts`] and the per-language `*_frontend()` factories.
pub struct LanguageFrontend {
    /// Parser identity + tree-sitter grammar.
    pub parser: Box<dyn ParserSpec>,
    /// Symbol definition extraction.
    pub symbols: Box<dyn SymbolExtractorSpec>,
    /// Reference use extraction.
    pub references: Box<dyn ReferenceExtractorSpec>,
    /// Import extraction.
    pub imports: Box<dyn ImportExtractorSpec>,
    /// Scope extraction.
    pub scopes: Box<dyn ScopeExtractorSpec>,
    /// Callsite extraction (language-specific AST walk).
    pub callsites: Box<dyn CallsiteExtractorSpec>,
    /// Lexical binding extraction.
    pub lexical: Box<dyn LexicalBindingSpec>,
    /// Dataflow extraction.
    pub(crate) dataflow: Box<dyn DataflowSpec>,
    /// Language capability profile (used by TraceEngine for gating).
    pub capability: LanguageCapabilityProfile,
}

impl LanguageFrontend {
    /// Construct from pre-built slot implementations.
    ///
    /// This is the direct-construction path used by per-language `*_frontend()`
    /// factories.
    pub fn from_parts(parts: FrontendParts) -> Self {
        Self {
            parser: parts.parser,
            symbols: parts.symbols,
            references: parts.references,
            imports: parts.imports,
            scopes: parts.scopes,
            callsites: parts.callsites,
            lexical: parts.lexical,
            dataflow: parts.dataflow,
            capability: parts.capability,
        }
    }

    /// Convenience: the Language variant.
    pub fn language(&self) -> Language {
        self.parser.language()
    }

    /// Build a `FeatureMatrix` from the static capability profile.
    ///
    /// The `LanguageCapabilityProfile::features` field is the single
    /// authoritative source of truth for per-feature capability.  This
    /// method returns it directly.
    pub fn feature_matrix(&self) -> FeatureMatrix {
        self.capability.features.clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "typescript")]
    #[test]
    fn test_frontend_ts_capabilities() {
        let frontend = crate::languages::create_frontend(Language::TypeScript)
            .expect("TS frontend should exist");
        assert_eq!(frontend.language(), Language::TypeScript);
        assert!(frontend.symbols.capability().is_supported());
        assert!(frontend.references.capability().is_supported());
        assert!(frontend.dataflow.capability().is_supported());
        assert!(frontend.lexical.capability().is_supported());
    }

    #[cfg(feature = "python")]
    #[test]
    fn test_frontend_python_capabilities() {
        let frontend = crate::languages::create_frontend(Language::Python)
            .expect("Python frontend should exist");
        assert_eq!(frontend.language(), Language::Python);
        assert!(frontend.dataflow.capability().is_supported());
        // Python lexical bindings are now supported (see capability.rs)
        assert!(frontend.lexical.capability().is_supported());
    }

    #[cfg(feature = "java")]
    #[test]
    fn test_frontend_java_capabilities() {
        let frontend =
            crate::languages::create_frontend(Language::Java).expect("Java frontend should exist");
        assert_eq!(frontend.language(), Language::Java);
        assert!(frontend.symbols.capability().is_supported());
        assert!(frontend.references.capability().is_supported());
        assert!(
            frontend.dataflow.capability().is_supported(),
            "Java dataflow should be supported (DataflowLocal)"
        );
        assert!(
            frontend.lexical.capability().is_supported(),
            "Java lexical should be supported (DataflowLocal)"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_frontend_feature_matrix_ts() {
        let frontend = crate::languages::create_frontend(Language::TypeScript)
            .expect("TS frontend should exist");
        let matrix = frontend.feature_matrix();

        assert!(matrix.local_dataflow.is_supported());
        assert!(matrix.lexical_bindings.is_supported());
        assert!(matrix.interprocedural_summaries.is_supported());
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_create_frontend_factory() {
        let frontend = crate::languages::create_frontend(Language::TypeScript)
            .expect("create_frontend should return TS frontend");
        assert_eq!(frontend.language(), Language::TypeScript);

        // Verify that all slot queries compile against tree-sitter grammar
        let ts_lang = frontend.parser.tree_sitter_language();
        for (name, query_src) in [
            ("definitions", frontend.symbols.definition_query()),
            ("references", frontend.references.reference_query()),
            ("imports", frontend.imports.import_query()),
            ("scopes", frontend.scopes.scope_query()),
        ] {
            let result = tree_sitter::Query::new(&ts_lang, query_src);
            assert!(
                result.is_ok(),
                "{name} query should parse: {:?}",
                result.err()
            );
        }

        // When capability is supported, query must also compile
        if frontend.lexical.capability().is_supported() {
            let q = tree_sitter::Query::new(&ts_lang, frontend.lexical.lexical_query());
            assert!(q.is_ok(), "lexical query should parse: {:?}", q.err());
        }
        if frontend.dataflow.capability().is_supported() {
            let q = tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query());
            assert!(
                q.is_ok(),
                "dataflow_builder query should parse: {:?}",
                q.err()
            );
        }
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_create_frontend_slot_normalize() {
        use std::path::Path;
        use tree_sitter::StreamingIterator;
        use types::ids::FileId;

        let frontend = crate::languages::create_frontend(Language::TypeScript)
            .expect("create_frontend should return TS frontend");
        let ts_lang = frontend.parser.tree_sitter_language();

        // Parse a small TS snippet
        let source = "function greet(name: string) { return name; }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        // Run definition query, normalize first capture
        let query = tree_sitter::Query::new(&ts_lang, frontend.symbols.definition_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.ts");
        let file_path = Path::new("test.ts");
        let ctx = NormalizeCtx {
            language: Language::TypeScript,
            file_id,
            file_path,
            source,
        };

        let mut matched = false;
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            let capture = Capture {
                name,
                node: cap.node,
            };
            if let Some(sym) = frontend.symbols.normalize(ctx, capture) {
                assert!(!sym.name.is_empty());
                assert!(!sym.qualified_name.is_empty());
                matched = true;
                break;
            }
        }
        assert!(
            matched,
            "definition query should match at least one symbol in TS source"
        );
    }

    /// Covers lexical and dataflow slot normalize paths that are easy to miss
    /// when only definition/reference/import are tested.
    #[cfg(feature = "typescript")]
    #[test]
    fn test_create_frontend_slot_normalize_lexical_dataflow() {
        use std::path::Path;
        use tree_sitter::StreamingIterator;
        use types::ids::FileId;

        let frontend = crate::languages::create_frontend(Language::TypeScript)
            .expect("create_frontend should return TS frontend");
        let ts_lang = frontend.parser.tree_sitter_language();

        // Source with lexical bindings (let/const/var) and a dataflow pattern
        // (assignment + return) so we exercise both slots.
        let source = "function f(x: number) { let y = x + 1; return y; }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let file_id = FileId::generate("test.ts");
        let file_path = Path::new("test.ts");
        let ctx = NormalizeCtx {
            language: Language::TypeScript,
            file_id,
            file_path,
            source,
        };

        // -- lexical slot --
        if frontend.lexical.capability().is_supported() {
            let q = tree_sitter::Query::new(&ts_lang, frontend.lexical.lexical_query()).unwrap();
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut hits = 0;
            let mut captures = cursor.captures(&q, root, source.as_bytes());
            while let Some((m, idx)) = captures.next() {
                let cap = m.captures[*idx];
                let name = q.capture_names()[cap.index as usize].to_string();
                if frontend
                    .lexical
                    .normalize(
                        ctx,
                        Capture {
                            name,
                            node: cap.node,
                        },
                    )
                    .is_some()
                {
                    hits += 1;
                }
            }
            assert!(
                hits > 0,
                "lexical query should produce at least one normalized BindingDef"
            );
        }

        // -- dataflow slot --
        if frontend.dataflow.capability().is_supported() {
            let q = tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query())
                .unwrap();
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut node_hits = 0usize;
            let mut captures = cursor.captures(&q, root, source.as_bytes());
            while let Some((m, idx)) = captures.next() {
                let cap = m.captures[*idx];
                let name = q.capture_names()[cap.index as usize].to_string();
                let (dn, _de) = frontend.dataflow.normalize(
                    ctx,
                    Capture {
                        name,
                        node: cap.node,
                    },
                );
                if dn.is_some() {
                    node_hits += 1;
                }
            }
            assert!(
                node_hits > 0,
                "dataflow query should produce at least one normalized DataNode"
            );
        }
    }

    /// B4: capability profile must agree with slot capabilities for every language.
    ///
    /// For each language reachable via `create_frontend()`:
    /// - If a slot declares itself unsupported, the capability profile must NOT
    ///   claim that category as supported.
    /// - Specific checks: dataflow, lexical, scopes.
    #[test]
    fn test_capability_profile_matches_slot_capabilities() {
        let languages = crate::languages::available_languages();

        for lang in languages {
            let frontend = crate::languages::create_frontend(lang)
                .unwrap_or_else(|| panic!("create_frontend failed for {lang:?}"));

            let profile = frontend.capability;
            let fm = &profile.features;

            // dataflow: slot says unsupported → profile must not claim main dataflow support
            if !frontend.dataflow.capability().is_supported() {
                assert!(
                    !fm.local_dataflow.is_supported(),
                    "{lang:?}: dataflow slot unsupported but profile claims local_dataflow"
                );
            }

            // lexical: slot says unsupported → profile must not claim lexical_bindings
            if !frontend.lexical.capability().is_supported() {
                assert!(
                    !fm.lexical_bindings.is_supported(),
                    "{lang:?}: lexical slot unsupported but profile claims lexical_bindings"
                );
            }

            // scopes: slot says unsupported → profile must not claim scopes
            if !frontend.scopes.capability().is_supported() {
                assert!(
                    !fm.scopes.is_supported(),
                    "{lang:?}: scope slot unsupported but profile claims scopes"
                );
            }
        }
    }

    #[test]
    fn test_scope_chain_identity_across_static_language_adapters() {
        let cases = vec![
            #[cfg(feature = "typescript")]
            (
                "scope.ts",
                Language::TypeScript,
                "function shadowTypescript(input: number): number {\n  let value = input;\n  if (input > 0) {\n    let value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "javascript")]
            (
                "scope.js",
                Language::JavaScript,
                "function shadowJavascript(input) {\n  let value = input;\n  if (input > 0) {\n    let value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "arkts")]
            (
                "scope.ets",
                Language::ArkTS,
                "function shadowArkts(input: number): number {\n  let value: number = input;\n  if (input > 0) {\n    let value: number = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "c")]
            (
                "scope.c",
                Language::C,
                "int shadow_c(int input) {\n  int value = input;\n  if (input > 0) {\n    int value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "cpp")]
            (
                "scope.cpp",
                Language::Cpp,
                "int shadow_cpp(int input) {\n  int value = input;\n  if (input > 0) {\n    int value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "java")]
            (
                "ScopeJava.java",
                Language::Java,
                "class ScopeJava {\n  static int shadowJava(int input, boolean first) {\n    if (first) {\n      int value = input;\n      consume(value);\n    } else {\n      int value = input + 1;\n      consume(value);\n    }\n    return input;\n  }\n}\n",
                "value",
                [(3, 4), (6, 7)],
            ),
            #[cfg(feature = "go")]
            (
                "scope.go",
                Language::Go,
                "package scope\n\nfunc shadowGo(input int) int {\n  value := input\n  if input > 0 {\n    value := input + 1\n    consume(value)\n  }\n  return value\n}\n",
                "value",
                [(3, 8), (5, 6)],
            ),
            #[cfg(feature = "rust")]
            (
                "scope.rs",
                Language::Rust,
                "fn shadow_rust(input: i32) -> i32 {\n  let value = input;\n  if input > 0 {\n    let value = input + 1;\n    consume(value);\n  }\n  value\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "kotlin")]
            (
                "ScopeKotlin.kt",
                Language::Kotlin,
                "fun shadowKotlin(input: Int): Int {\n  val value = input\n  if (input > 0) {\n    val value = input + 1\n    consume(value)\n  }\n  return value\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
            #[cfg(feature = "cangjie")]
            (
                "scope.cj",
                Language::Cangjie,
                "func shadowCangjie(input: Int64): Int64 {\n  let value = input\n  if (input > 0) {\n    let value = input + 1\n    consume(value)\n  }\n  return value\n}\n",
                "value",
                [(1, 6), (3, 4)],
            ),
        ];

        for (path, language, source, name, declaration_and_use_lines) in cases {
            let frontend = crate::languages::create_frontend(language)
                .unwrap_or_else(|| panic!("missing {language:?} frontend"));
            let file_id = FileId::generate(path);
            let facts = crate::extract_file_with_mode(
                &frontend,
                file_id,
                std::path::Path::new(path),
                source,
                "hash",
                crate::ExtractionMode::Full,
                &(),
            )
            .unwrap_or_else(|error| panic!("{language:?} extraction failed: {error:#}"));

            let mut bindings: Vec<_> = facts
                .bindings
                .iter()
                .filter(|binding| binding.name == name)
                .collect();
            bindings.sort_by_key(|binding| binding.range.start_byte);
            assert_eq!(
                bindings.len(),
                2,
                "{language:?}: expected two {name} bindings"
            );
            assert_ne!(bindings[0].id, bindings[1].id, "{language:?}");
            assert_ne!(bindings[0].scope_id, bindings[1].scope_id, "{language:?}");
            assert_eq!(
                bindings[0].function_id, bindings[1].function_id,
                "{language:?}"
            );
            assert!(bindings[0].function_id.is_some(), "{language:?}");

            for ((declaration_line, use_line), binding) in
                declaration_and_use_lines.into_iter().zip(bindings)
            {
                assert_eq!(binding.range.start_line, declaration_line, "{language:?}");
                assert!(
                    facts.binding_uses.iter().any(|use_| {
                        use_.name == name
                            && use_.range.start_line == use_line
                            && use_.binding_id == Some(binding.id)
                    }),
                    "{language:?}: line {use_line} must resolve binding declared on line {declaration_line}"
                );
                assert!(
                    facts.data_nodes.iter().any(|node| {
                        node.kind == types::DataNodeKind::VariableUse
                            && node.name.as_deref() == Some(name)
                            && node.range.start_line == use_line
                            && node.binding_id == Some(binding.id)
                    }),
                    "{language:?}: data node on line {use_line} must keep binding identity"
                );
            }
        }
    }

    #[test]
    fn test_typescript_family_variable_mutations_preserve_read_modify_write_provenance() {
        let cases = vec![
            #[cfg(feature = "typescript")]
            (
                "variable_mutations.ts",
                Language::TypeScript,
                "function mutate(seed: number, delta: number): number {\n  let total = seed;\n  total += delta;\n  total++;\n  --total;\n  items[0] += delta;\n  items[1]++;\n  return total;\n}\n",
            ),
            #[cfg(feature = "javascript")]
            (
                "variable_mutations.js",
                Language::JavaScript,
                "function mutate(seed, delta) {\n  let total = seed;\n  total += delta;\n  total++;\n  --total;\n  items[0] += delta;\n  items[1]++;\n  return total;\n}\n",
            ),
            #[cfg(feature = "arkts")]
            (
                "variable_mutations.ets",
                Language::ArkTS,
                "function mutate(seed: number, delta: number): number {\n  let total: number = seed;\n  total += delta;\n  total++;\n  --total;\n  items[0] += delta;\n  items[1]++;\n  return total;\n}\n",
            ),
        ];

        for (path, language, source) in cases {
            let frontend = crate::languages::create_frontend(language)
                .unwrap_or_else(|| panic!("missing {language:?} frontend"));
            let facts = crate::extract_file_with_mode(
                &frontend,
                FileId::generate(path),
                std::path::Path::new(path),
                source,
                "hash",
                crate::ExtractionMode::Full,
                &(),
            )
            .unwrap_or_else(|error| panic!("{language:?} extraction failed: {error:#}"));

            let total_binding = {
                let matches: Vec<_> = facts
                    .bindings
                    .iter()
                    .filter(|binding| binding.name == "total")
                    .collect();
                assert_eq!(
                    matches.len(),
                    1,
                    "{language:?}: mutation writes must reuse the declaration binding"
                );
                matches[0]
            };
            let delta_binding = facts
                .bindings
                .iter()
                .find(|binding| binding.name == "delta")
                .unwrap_or_else(|| panic!("{language:?}: delta parameter binding"));

            let node = |kind: types::DataNodeKind, name: &str, line: u32| {
                facts
                    .data_nodes
                    .iter()
                    .find(|node| {
                        node.kind == kind
                            && node.name.as_deref() == Some(name)
                            && node.range.start_line == line
                    })
                    .unwrap_or_else(|| {
                        panic!("{language:?}: missing {kind:?} {name} on line {line}")
                    })
            };
            for (line, expression) in [(2, "total += delta"), (3, "total++"), (4, "--total")] {
                let target = node(types::DataNodeKind::Local, "total", line);
                let value = node(types::DataNodeKind::Expr, expression, line);
                let lhs_read = node(types::DataNodeKind::VariableUse, "total", line);
                assert_eq!(target.binding_id, Some(total_binding.id), "{language:?}");
                assert_eq!(lhs_read.binding_id, Some(total_binding.id), "{language:?}");
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == value.id
                            && edge.target == target.id
                            && edge.kind == types::DataFlowKind::Assign
                            && edge.confidence == 0.90
                    }),
                    "{language:?}: mutation aggregate {expression} on line {line} must flow to its target"
                );
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == lhs_read.id
                            && edge.target == value.id
                            && edge.kind == types::DataFlowKind::Read
                            && edge.confidence == 0.75
                    }),
                    "{language:?}: previous target value must flow into {expression} on line {line}"
                );
            }

            let rhs_read = node(types::DataNodeKind::VariableUse, "delta", 2);
            let compound_value = node(types::DataNodeKind::Expr, "total += delta", 2);
            assert_eq!(rhs_read.binding_id, Some(delta_binding.id), "{language:?}");
            assert!(
                facts.dataflow_edges.iter().any(|edge| {
                    edge.source == rhs_read.id
                        && edge.target == compound_value.id
                        && edge.kind == types::DataFlowKind::Read
                        && edge.confidence == 0.75
                }),
                "{language:?}: compound-assignment RHS must flow into mutation aggregate"
            );

            assert!(
                facts.data_nodes.iter().all(|node| {
                    !(matches!(
                        node.kind,
                        types::DataNodeKind::Local | types::DataNodeKind::Expr
                    ) && matches!(node.range.start_line, 5 | 6))
                }),
                "{language:?}: member/subscript mutations remain outside the direct-variable boundary"
            );
        }
    }

    #[test]
    fn test_typescript_family_logical_assignments_preserve_may_provenance() {
        let cases = vec![
            #[cfg(feature = "typescript")]
            (
                "logical_assignments.ts",
                Language::TypeScript,
                "function initialize(seed: number | undefined, fallback: number, guard: number): number {\n  let value = seed;\n  value ??= fallback;\n  value ||= fallback;\n  value &&= guard;\n  holder.value ??= fallback;\n  items[0] ||= guard;\n  return value;\n}\n",
            ),
            #[cfg(feature = "javascript")]
            (
                "logical_assignments.js",
                Language::JavaScript,
                "function initialize(seed, fallback, guard) {\n  let value = seed;\n  value ??= fallback;\n  value ||= fallback;\n  value &&= guard;\n  holder.value ??= fallback;\n  items[0] ||= guard;\n  return value;\n}\n",
            ),
            #[cfg(feature = "arkts")]
            (
                "logical_assignments.ets",
                Language::ArkTS,
                "function initialize(seed: number | undefined, fallback: number, guard: number): number {\n  let value: number | undefined = seed;\n  value ??= fallback;\n  value ||= fallback;\n  value &&= guard;\n  holder.value ??= fallback;\n  items[0] ||= guard;\n  return value;\n}\n",
            ),
        ];

        for (path, language, source) in cases {
            let frontend = crate::languages::create_frontend(language)
                .unwrap_or_else(|| panic!("missing {language:?} frontend"));
            let facts = crate::extract_file_with_mode(
                &frontend,
                FileId::generate(path),
                std::path::Path::new(path),
                source,
                "hash",
                crate::ExtractionMode::Full,
                &(),
            )
            .unwrap_or_else(|error| panic!("{language:?} extraction failed: {error:#}"));

            let value_binding = {
                let matches: Vec<_> = facts
                    .bindings
                    .iter()
                    .filter(|binding| binding.name == "value")
                    .collect();
                assert_eq!(matches.len(), 1, "{language:?}: one value binding");
                matches[0]
            };
            let node = |kind: types::DataNodeKind, name: &str, line: u32| {
                facts
                    .data_nodes
                    .iter()
                    .find(|node| {
                        node.kind == kind
                            && node.name.as_deref() == Some(name)
                            && node.range.start_line == line
                    })
                    .unwrap_or_else(|| {
                        panic!("{language:?}: missing {kind:?} {name} on line {line}")
                    })
            };

            for (line, expression, rhs_name) in [
                (2, "value ??= fallback", "fallback"),
                (3, "value ||= fallback", "fallback"),
                (4, "value &&= guard", "guard"),
            ] {
                let merged_value = node(types::DataNodeKind::Expr, expression, line);
                let target = node(types::DataNodeKind::Local, "value", line);
                let old_value = node(types::DataNodeKind::VariableUse, "value", line);
                let conditional_rhs = node(types::DataNodeKind::VariableUse, rhs_name, line);
                assert_eq!(target.binding_id, Some(value_binding.id), "{language:?}");
                assert_eq!(old_value.binding_id, Some(value_binding.id), "{language:?}");
                assert!(facts.dataflow_edges.iter().any(|edge| {
                    edge.source == merged_value.id
                        && edge.target == target.id
                        && edge.kind == types::DataFlowKind::Assign
                        && edge.confidence == 0.90
                }));
                for possible_origin in [old_value, conditional_rhs] {
                    assert!(
                        facts.dataflow_edges.iter().any(|edge| {
                            edge.source == possible_origin.id
                                && edge.target == merged_value.id
                                && edge.kind == types::DataFlowKind::Read
                                && edge.confidence == 0.75
                        }),
                        "{language:?}: {expression} must preserve possible origin {:?}",
                        possible_origin.name
                    );
                }
            }

            for (line, expression) in [(5, "holder.value ??= fallback"), (6, "items[0] ||= guard")]
            {
                assert!(
                    facts.data_nodes.iter().all(|node| {
                        !(matches!(
                            node.kind,
                            types::DataNodeKind::Local | types::DataNodeKind::Expr
                        ) && node.range.start_line == line)
                    }),
                    "{language:?}: unsupported logical target {expression} must not become a local write"
                );
            }
        }
    }

    #[test]
    fn test_typescript_family_for_in_bindings_receive_iterable_aggregate() {
        let cases = vec![
            #[cfg(feature = "typescript")]
            ("for_in.ts", Language::TypeScript),
            #[cfg(feature = "javascript")]
            ("for_in.js", Language::JavaScript),
            #[cfg(feature = "arkts")]
            ("for_in.ets", Language::ArkTS),
        ];
        let source = concat!(
            "async function select(value, rows, records, stream, values, record) {\n",
            "  let key = 'outer';\n",
            "  for (const [key, count] of rows) {\n",
            "    consume(key, count);\n",
            "  }\n",
            "  consume(key);\n",
            "  for (const { name, meta: { score } } of records) {\n",
            "    consume(name, score);\n",
            "  }\n",
            "  for await (const item of stream) {\n",
            "    consume(item);\n",
            "  }\n",
            "  for (value of values) {\n",
            "    consume(value);\n",
            "  }\n",
            "  for (const property in record) {\n",
            "    consume(property);\n",
            "  }\n",
            "  for (holder.value of values) {\n",
            "    consume(holder.value);\n",
            "  }\n",
            "  for (items[0] of values) {\n",
            "    consume(items[0]);\n",
            "  }\n",
            "  for (var legacy of values) {\n",
            "    consume(legacy);\n",
            "  }\n",
            "  return value;\n",
            "}\n",
        );

        for (path, language) in cases {
            let frontend = crate::languages::create_frontend(language)
                .unwrap_or_else(|| panic!("missing {language:?} frontend"));
            let facts = crate::extract_file_with_mode(
                &frontend,
                FileId::generate(path),
                std::path::Path::new(path),
                source,
                "hash",
                crate::ExtractionMode::Full,
                &(),
            )
            .unwrap_or_else(|error| panic!("{language:?} extraction failed: {error:#}"));

            let mut key_bindings: Vec<_> = facts
                .bindings
                .iter()
                .filter(|binding| binding.name == "key")
                .collect();
            key_bindings.sort_by_key(|binding| binding.range.start_byte);
            assert_eq!(key_bindings.len(), 2, "{language:?}: outer and loop key");
            let outer_key = key_bindings[0];
            let loop_key = key_bindings[1];
            assert_ne!(outer_key.id, loop_key.id, "{language:?}");
            assert_ne!(outer_key.scope_id, loop_key.scope_id, "{language:?}");
            assert_eq!(
                facts
                    .scopes
                    .iter()
                    .find(|scope| scope.id == loop_key.scope_id)
                    .map(|scope| scope.kind),
                Some(types::ScopeKind::Loop),
                "{language:?}: for binding must be loop-scoped"
            );
            assert!(facts.binding_uses.iter().any(|use_| {
                use_.name == "key"
                    && use_.range.start_line == 3
                    && use_.binding_id == Some(loop_key.id)
            }));
            assert!(facts.binding_uses.iter().any(|use_| {
                use_.name == "key"
                    && use_.range.start_line == 5
                    && use_.binding_id == Some(outer_key.id)
            }));
            assert!(
                facts
                    .bindings
                    .iter()
                    .all(|binding| !matches!(binding.name.as_str(), "meta" | "legacy")),
                "{language:?}: property keys and var-loop targets must not become loop-scoped bindings"
            );

            let value_binding = facts
                .bindings
                .iter()
                .find(|binding| binding.name == "value")
                .unwrap_or_else(|| panic!("{language:?}: value parameter binding"));
            assert_eq!(
                facts
                    .bindings
                    .iter()
                    .filter(|binding| binding.name == "value")
                    .count(),
                1,
                "{language:?}: assignment-form loop must reuse the existing binding"
            );

            let binding_id = |name: &str| {
                facts
                    .bindings
                    .iter()
                    .find(|binding| binding.name == name)
                    .unwrap_or_else(|| panic!("{language:?}: missing binding {name}"))
                    .id
            };
            for (iterable_name, loop_line, target_name, binding_id) in [
                ("rows", 2, "key", loop_key.id),
                ("rows", 2, "count", binding_id("count")),
                ("records", 6, "name", binding_id("name")),
                ("records", 6, "score", binding_id("score")),
                ("stream", 9, "item", binding_id("item")),
                ("values", 12, "value", value_binding.id),
                ("record", 15, "property", binding_id("property")),
            ] {
                let iterable = facts
                    .data_nodes
                    .iter()
                    .find(|node| {
                        node.kind == types::DataNodeKind::Expr
                            && node.name.as_deref() == Some(iterable_name)
                            && node.range.start_line == loop_line
                    })
                    .unwrap_or_else(|| {
                        panic!("{language:?}: missing iterable {iterable_name} on line {loop_line}")
                    });
                let target = facts
                    .data_nodes
                    .iter()
                    .find(|node| {
                        node.kind == types::DataNodeKind::Local
                            && node.name.as_deref() == Some(target_name)
                            && node.range.start_line == loop_line
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{language:?}: missing loop target {target_name} on line {loop_line}"
                        )
                    });
                assert_eq!(target.binding_id, Some(binding_id), "{language:?}");
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == iterable.id
                            && edge.target == target.id
                            && edge.kind == types::DataFlowKind::Assign
                            && edge.confidence == 0.65
                    }),
                    "{language:?}: {iterable_name} must provide aggregate provenance to {target_name}"
                );
            }

            for line in [18, 21] {
                assert!(
                    facts.data_nodes.iter().all(|node| {
                        node.kind != types::DataNodeKind::Local || node.range.start_line != line
                    }),
                    "{language:?}: member/subscript loop targets remain conservative"
                );
            }
            for line in [2, 6, 9, 15] {
                assert!(
                    facts.data_nodes.iter().all(|node| {
                        node.kind != types::DataNodeKind::VariableUse
                            || node.range.start_line != line
                            || !matches!(
                                node.name.as_deref(),
                                Some("key" | "count" | "name" | "score" | "item" | "property")
                            )
                    }),
                    "{language:?}: declaration-form loop targets must not be modeled as reads"
                );
            }
        }
    }

    #[test]
    fn test_c_style_variable_mutations_preserve_read_modify_write_provenance() {
        let cases = vec![
            #[cfg(feature = "c")]
            (
                "variable_mutations.c",
                Language::C,
                "int mutate(int seed, int delta) {\n  int total = seed;\n  total += delta;\n  total++;\n  --total;\n  holder.value += delta;\n  items[0] += delta;\n  items[1]++;\n  return total;\n}\n",
                2,
            ),
            #[cfg(feature = "cpp")]
            (
                "variable_mutations.cpp",
                Language::Cpp,
                "int mutate(int seed, int delta) {\n  int total = seed;\n  total += delta;\n  total++;\n  --total;\n  holder.value += delta;\n  items[0] += delta;\n  items[1]++;\n  return total;\n}\n",
                2,
            ),
            #[cfg(feature = "java")]
            (
                "Mutation.java",
                Language::Java,
                "class Mutation {\n  static int mutate(int seed, int delta) {\n    int total = seed;\n    total += delta;\n    total++;\n    --total;\n    holder.value += delta;\n    items[0] += delta;\n    items[1]++;\n    return total;\n  }\n}\n",
                3,
            ),
            #[cfg(feature = "csharp")]
            (
                "Mutation.cs",
                Language::CSharp,
                "class Mutation {\n  static int Mutate(int seed, int delta) {\n    int total = seed;\n    total += delta;\n    total++;\n    --total;\n    holder.value += delta;\n    items[0] += delta;\n    items[1]++;\n    return total;\n  }\n}\n",
                3,
            ),
        ];

        for (path, language, source, mutation_line) in cases {
            let frontend = crate::languages::create_frontend(language)
                .unwrap_or_else(|| panic!("missing {language:?} frontend"));
            let facts = crate::extract_file_with_mode(
                &frontend,
                FileId::generate(path),
                std::path::Path::new(path),
                source,
                "hash",
                crate::ExtractionMode::Full,
                &(),
            )
            .unwrap_or_else(|error| panic!("{language:?} extraction failed: {error:#}"));

            let total_binding = {
                let matches: Vec<_> = facts
                    .bindings
                    .iter()
                    .filter(|binding| binding.name == "total")
                    .collect();
                assert_eq!(
                    matches.len(),
                    1,
                    "{language:?}: mutation writes must reuse the declaration binding"
                );
                matches[0]
            };
            let delta_binding = facts
                .bindings
                .iter()
                .find(|binding| binding.name == "delta")
                .unwrap_or_else(|| panic!("{language:?}: delta parameter binding"));
            let node = |kind: types::DataNodeKind, name: &str, line: u32| {
                facts
                    .data_nodes
                    .iter()
                    .find(|node| {
                        node.kind == kind
                            && node.name.as_deref() == Some(name)
                            && node.range.start_line == line
                    })
                    .unwrap_or_else(|| {
                        panic!("{language:?}: missing {kind:?} {name} on line {line}")
                    })
            };

            for (line, expression) in [
                (mutation_line, "total += delta"),
                (mutation_line + 1, "total++"),
                (mutation_line + 2, "--total"),
            ] {
                let target = node(types::DataNodeKind::Local, "total", line);
                let value = node(types::DataNodeKind::Expr, expression, line);
                let lhs_read = node(types::DataNodeKind::VariableUse, "total", line);
                assert_eq!(target.binding_id, Some(total_binding.id), "{language:?}");
                assert_eq!(lhs_read.binding_id, Some(total_binding.id), "{language:?}");
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == value.id
                            && edge.target == target.id
                            && edge.kind == types::DataFlowKind::Assign
                            && edge.confidence == 0.90
                    }),
                    "{language:?}: mutation aggregate {expression} on line {line} must flow to its target"
                );
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == lhs_read.id
                            && edge.target == value.id
                            && edge.kind == types::DataFlowKind::Read
                            && edge.confidence == 0.75
                    }),
                    "{language:?}: previous target value must flow into {expression} on line {line}"
                );
            }

            let compound_value = node(types::DataNodeKind::Expr, "total += delta", mutation_line);
            let rhs_read = node(types::DataNodeKind::VariableUse, "delta", mutation_line);
            assert_eq!(rhs_read.binding_id, Some(delta_binding.id), "{language:?}");
            assert!(
                facts.dataflow_edges.iter().any(|edge| {
                    edge.source == rhs_read.id
                        && edge.target == compound_value.id
                        && edge.kind == types::DataFlowKind::Read
                        && edge.confidence == 0.75
                }),
                "{language:?}: compound-assignment RHS must flow into mutation aggregate"
            );
            let rhs_value = node(types::DataNodeKind::Expr, "delta", mutation_line);
            let compound_target = node(types::DataNodeKind::Local, "total", mutation_line);
            assert!(
                facts.dataflow_edges.iter().all(|edge| {
                    !(edge.source == rhs_value.id
                        && edge.target == compound_target.id
                        && edge.kind == types::DataFlowKind::Assign)
                }),
                "{language:?}: compound RHS alone must not be treated as the assigned value"
            );

            let member_line = mutation_line + 3;
            assert!(
                facts.data_nodes.iter().all(|node| {
                    !(node.kind == types::DataNodeKind::Expr
                        && node.name.as_deref() == Some("holder.value += delta")
                        && node.range.start_line == member_line)
                }),
                "{language:?}: member mutation remains outside the direct-variable boundary"
            );
            assert!(
                facts.dataflow_edges.iter().all(|edge| {
                    !(edge.kind == types::DataFlowKind::FieldStore
                        && edge.location.start_line == member_line)
                }),
                "{language:?}: member compound assignment must not degrade to an RHS-only store"
            );

            for (line, expression) in [
                (mutation_line + 4, "items[0] += delta"),
                (mutation_line + 5, "items[1]++"),
            ] {
                assert!(
                    facts.data_nodes.iter().all(|node| {
                        !(node.kind == types::DataNodeKind::Expr
                            && node.name.as_deref() == Some(expression)
                            && node.range.start_line == line)
                    }),
                    "{language:?}: subscript mutation remains outside the direct-variable boundary"
                );
            }
        }
    }

    #[test]
    fn test_remaining_language_variable_mutations_preserve_read_modify_write_provenance() {
        struct MutationCase<'a> {
            path: &'a str,
            language: Language,
            source: &'a str,
            mutations: &'a [(u32, &'a str)],
            compound_line: u32,
            conservative: &'a [(u32, &'a str)],
        }

        let cases = vec![
            #[cfg(feature = "python")]
            MutationCase {
                path: "variable_mutations.py",
                language: Language::Python,
                source: "def mutate(seed, delta):\n    total = seed\n    total += delta\n    holder.value += delta\n    items[0] += delta\n    return total\n",
                mutations: &[(2, "total += delta")],
                compound_line: 2,
                conservative: &[(3, "holder.value += delta"), (4, "items[0] += delta")],
            },
            #[cfg(feature = "go")]
            MutationCase {
                path: "variable_mutations.go",
                language: Language::Go,
                source: "package mutations\n\nfunc mutate(seed int, delta int) int {\n  total := seed\n  total += delta\n  total++\n  total--\n  holder.value += delta\n  items[0] += delta\n  items[1]++\n  return total\n}\n",
                mutations: &[(4, "total += delta"), (5, "total++"), (6, "total--")],
                compound_line: 4,
                conservative: &[
                    (7, "holder.value += delta"),
                    (8, "items[0] += delta"),
                    (9, "items[1]++"),
                ],
            },
            #[cfg(feature = "rust")]
            MutationCase {
                path: "variable_mutations.rs",
                language: Language::Rust,
                source: "fn mutate(seed: i32, delta: i32) -> i32 {\n    let mut total = seed;\n    total += delta;\n    holder.value += delta;\n    items[0] += delta;\n    total\n}\n",
                mutations: &[(2, "total += delta")],
                compound_line: 2,
                conservative: &[(3, "holder.value += delta"), (4, "items[0] += delta")],
            },
            #[cfg(feature = "kotlin")]
            MutationCase {
                path: "VariableMutations.kt",
                language: Language::Kotlin,
                source: "fun mutate(seed: Int, delta: Int): Int {\n    var total = seed\n    total += delta\n    total++\n    --total\n    holder.value += delta\n    items[0] += delta\n    items[1]++\n    return total\n}\n",
                mutations: &[(2, "total += delta"), (3, "total++"), (4, "--total")],
                compound_line: 2,
                conservative: &[
                    (5, "holder.value += delta"),
                    (6, "items[0] += delta"),
                    (7, "items[1]++"),
                ],
            },
            #[cfg(feature = "ruby")]
            MutationCase {
                path: "variable_mutations.rb",
                language: Language::Ruby,
                source: "def mutate(seed, delta)\n  total = seed\n  total += delta\n  holder.value += delta\n  items[0] += delta\n  total ||= delta\n  total\nend\n",
                mutations: &[(2, "total += delta")],
                compound_line: 2,
                conservative: &[
                    (3, "holder.value += delta"),
                    (4, "items[0] += delta"),
                    (5, "total ||= delta"),
                ],
            },
        ];

        for case in cases {
            let MutationCase {
                path,
                language,
                source,
                mutations,
                compound_line,
                conservative,
            } = case;
            let frontend = crate::languages::create_frontend(language)
                .unwrap_or_else(|| panic!("missing {language:?} frontend"));
            let facts = crate::extract_file_with_mode(
                &frontend,
                FileId::generate(path),
                std::path::Path::new(path),
                source,
                "hash",
                crate::ExtractionMode::Full,
                &(),
            )
            .unwrap_or_else(|error| panic!("{language:?} extraction failed: {error:#}"));

            let total_binding = {
                let matches: Vec<_> = facts
                    .bindings
                    .iter()
                    .filter(|binding| binding.name == "total")
                    .collect();
                assert_eq!(
                    matches.len(),
                    1,
                    "{language:?}: mutation writes must reuse the declaration binding"
                );
                matches[0]
            };
            let delta_binding = facts
                .bindings
                .iter()
                .find(|binding| binding.name == "delta")
                .unwrap_or_else(|| panic!("{language:?}: delta parameter binding"));
            let node = |kind: types::DataNodeKind, name: &str, line: u32| {
                facts
                    .data_nodes
                    .iter()
                    .find(|node| {
                        node.kind == kind
                            && node.name.as_deref() == Some(name)
                            && node.range.start_line == line
                    })
                    .unwrap_or_else(|| {
                        panic!("{language:?}: missing {kind:?} {name} on line {line}")
                    })
            };

            for &(line, expression) in mutations {
                let target = node(types::DataNodeKind::Local, "total", line);
                let value = node(types::DataNodeKind::Expr, expression, line);
                let lhs_read = node(types::DataNodeKind::VariableUse, "total", line);
                assert_eq!(target.binding_id, Some(total_binding.id), "{language:?}");
                assert_eq!(lhs_read.binding_id, Some(total_binding.id), "{language:?}");
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == value.id
                            && edge.target == target.id
                            && edge.kind == types::DataFlowKind::Assign
                            && edge.confidence == 0.90
                    }),
                    "{language:?}: mutation aggregate {expression} on line {line} must flow to its target"
                );
                assert!(
                    facts.dataflow_edges.iter().any(|edge| {
                        edge.source == lhs_read.id
                            && edge.target == value.id
                            && edge.kind == types::DataFlowKind::Read
                            && edge.confidence == 0.75
                    }),
                    "{language:?}: previous target value must flow into {expression} on line {line}"
                );
            }

            let compound_value = node(types::DataNodeKind::Expr, "total += delta", compound_line);
            let compound_target = node(types::DataNodeKind::Local, "total", compound_line);
            let rhs_read = node(types::DataNodeKind::VariableUse, "delta", compound_line);
            let rhs_value = node(types::DataNodeKind::Expr, "delta", compound_line);
            assert_eq!(rhs_read.binding_id, Some(delta_binding.id), "{language:?}");
            assert!(
                facts.dataflow_edges.iter().any(|edge| {
                    edge.source == rhs_read.id
                        && edge.target == compound_value.id
                        && edge.kind == types::DataFlowKind::Read
                        && edge.confidence == 0.75
                }),
                "{language:?}: compound-assignment RHS must flow into the mutation aggregate"
            );
            assert!(
                facts.dataflow_edges.iter().all(|edge| {
                    !(edge.source == rhs_value.id
                        && edge.target == compound_target.id
                        && edge.kind == types::DataFlowKind::Assign)
                }),
                "{language:?}: compound RHS alone must not be treated as the assigned value"
            );

            for &(line, expression) in conservative {
                assert!(
                    facts.data_nodes.iter().all(|node| {
                        !(node.kind == types::DataNodeKind::Expr
                            && node.name.as_deref() == Some(expression)
                            && node.range.start_line == line)
                    }),
                    "{language:?}: unsupported mutation {expression} remains outside the direct-variable boundary"
                );
                assert!(
                    facts.data_nodes.iter().all(|node| {
                        !(node.kind == types::DataNodeKind::Local && node.range.start_line == line)
                    }),
                    "{language:?}: unsupported mutation {expression} must not create a fake local write"
                );
                assert!(
                    facts.dataflow_edges.iter().all(|edge| {
                        !(edge.kind == types::DataFlowKind::FieldStore
                            && edge.location.start_line == line)
                    }),
                    "{language:?}: unsupported compound target must not degrade to an RHS-only field store"
                );
            }
        }
    }

    #[cfg(feature = "cangjie")]
    #[test]
    fn test_cangjie_variable_reassignment_and_mutations_preserve_provenance() {
        let source = concat!(
            "func mutate(seed: Int64, delta: Int64, guard: Bool): Int64 {\n",
            "    var total = seed\n",
            "    total = delta\n",
            "    total += delta\n",
            "    total++\n",
            "    total--\n",
            "    holder.value += delta\n",
            "    items[0] += delta\n",
            "    items[1]++\n",
            "    var flag = true\n",
            "    flag &&= guard\n",
            "    flag ||= guard\n",
            "    return total\n",
            "}\n",
        );
        let frontend =
            crate::languages::create_frontend(Language::Cangjie).expect("missing Cangjie frontend");
        let language = frontend.parser.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "fixture must parse with the pinned Cangjie grammar: {}",
            tree.root_node().to_sexp()
        );

        let facts = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("variable_mutations.cj"),
            std::path::Path::new("variable_mutations.cj"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .expect("Cangjie extraction");

        let total_binding = {
            let matches: Vec<_> = facts
                .bindings
                .iter()
                .filter(|binding| binding.name == "total")
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "writes must reuse the declaration binding"
            );
            matches[0]
        };
        let delta_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "delta")
            .expect("delta parameter binding");
        let node = |kind: types::DataNodeKind, name: &str, line: u32| {
            facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == kind
                        && node.name.as_deref() == Some(name)
                        && node.range.start_line == line
                })
                .unwrap_or_else(|| panic!("missing {kind:?} {name} on line {line}"))
        };

        let reassignment_target = node(types::DataNodeKind::Local, "total", 2);
        let reassignment_value = node(types::DataNodeKind::Expr, "delta", 2);
        assert_eq!(reassignment_target.binding_id, Some(total_binding.id));
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == reassignment_value.id
                && edge.target == reassignment_target.id
                && edge.kind == types::DataFlowKind::Assign
                && edge.confidence == 0.90
        }));
        assert!(
            facts.data_nodes.iter().all(|node| {
                !(node.kind == types::DataNodeKind::VariableUse
                    && node.name.as_deref() == Some("total")
                    && node.range.start_line == 2)
            }),
            "simple-assignment target must not become a read"
        );

        for (line, expression) in [(3, "total += delta"), (4, "total++"), (5, "total--")] {
            let target = node(types::DataNodeKind::Local, "total", line);
            let value = node(types::DataNodeKind::Expr, expression, line);
            let lhs_read = node(types::DataNodeKind::VariableUse, "total", line);
            assert_eq!(target.binding_id, Some(total_binding.id));
            assert_eq!(lhs_read.binding_id, Some(total_binding.id));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == value.id
                    && edge.target == target.id
                    && edge.kind == types::DataFlowKind::Assign
                    && edge.confidence == 0.90
            }));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == lhs_read.id
                    && edge.target == value.id
                    && edge.kind == types::DataFlowKind::Read
                    && edge.confidence == 0.75
            }));
        }

        let rhs_read = node(types::DataNodeKind::VariableUse, "delta", 3);
        let compound_value = node(types::DataNodeKind::Expr, "total += delta", 3);
        assert_eq!(rhs_read.binding_id, Some(delta_binding.id));
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == rhs_read.id
                && edge.target == compound_value.id
                && edge.kind == types::DataFlowKind::Read
                && edge.confidence == 0.75
        }));

        for (line, expression) in [
            (6, "holder.value += delta"),
            (7, "items[0] += delta"),
            (8, "items[1]++"),
            (10, "flag &&= guard"),
            (11, "flag ||= guard"),
        ] {
            assert!(
                facts.data_nodes.iter().all(|node| {
                    !(node.kind == types::DataNodeKind::Expr
                        && node.name.as_deref() == Some(expression)
                        && node.range.start_line == line)
                }),
                "unsupported mutation {expression} remains outside the direct boundary"
            );
            assert!(facts.data_nodes.iter().all(|node| {
                !(node.kind == types::DataNodeKind::Local && node.range.start_line == line)
            }));
            assert!(facts.dataflow_edges.iter().all(|edge| {
                !(edge.kind == types::DataFlowKind::FieldStore && edge.location.start_line == line)
            }));
        }
    }
}

/// Verify that every language with lexical support has a compilable lexical query.
#[test]
fn test_all_lexical_queries_compile() {
    use crate::languages::{available_languages, create_frontend};

    for lang in available_languages() {
        let Some(frontend) = create_frontend(lang) else {
            continue;
        };
        if !frontend.lexical.capability().is_supported() {
            continue;
        }
        let ts_lang = frontend.parser.tree_sitter_language();
        let query_src = frontend.lexical.lexical_query();
        let q = tree_sitter::Query::new(&ts_lang, query_src);
        assert!(
            q.is_ok(),
            "{:?} lexical query must compile: {:?}",
            lang,
            q.err()
        );
    }
}

/// Verify that each dataflow language produces at least some DataNodes and edges.
#[test]
fn test_all_dataflow_languages_produce_facts() {
    use crate::languages::{available_languages, create_frontend};
    use crate::{ExtractionMode, extract_file_with_mode};
    use std::collections::HashSet;
    use types::enums::Language;
    use types::ids::FileId;

    let fixtures: &[(&str, Language, &str)] = &[
        #[cfg(feature = "typescript")]
        (
            "const x = 1;\nfunction f(a: number) { return a + x; }\n",
            Language::TypeScript,
            "ts",
        ),
        #[cfg(feature = "javascript")]
        (
            "const x = 1;\nfunction f(a) { return a + x; }\n",
            Language::JavaScript,
            "js",
        ),
        #[cfg(feature = "python")]
        (
            "def f(a):\n    x = 1\n    return a + x\n",
            Language::Python,
            "py",
        ),
        #[cfg(feature = "java")]
        (
            "class C { int f(int a) { int x = 1; return a + x; } }\n",
            Language::Java,
            "java",
        ),
        #[cfg(feature = "c")]
        (
            "int f(int a) { int x = 1; return a + x; }\n",
            Language::C,
            "c",
        ),
        #[cfg(feature = "cpp")]
        (
            "int f(int a) { int x = 1; return a + x; }\n",
            Language::Cpp,
            "cpp",
        ),
        #[cfg(feature = "arkts")]
        (
            "function f(a: number): number { const x = 1; return a + x; }\n",
            Language::ArkTS,
            "ets",
        ),
        #[cfg(feature = "cangjie")]
        (
            "func f(a: Int64): Int64 {\n    let x = 1\n    return a + x\n}\n",
            Language::Cangjie,
            "cj",
        ),
        #[cfg(feature = "go")]
        (
            "package p\nfunc f(a int) int { x := 1; return a + x }\n",
            Language::Go,
            "go",
        ),
        #[cfg(feature = "csharp")]
        (
            "class C { int F(int a) { int x = 1; return a + x; } }\n",
            Language::CSharp,
            "cs",
        ),
        #[cfg(feature = "rust")]
        (
            "fn f(a: i32) -> i32 { let x = 1; a + x }\n",
            Language::Rust,
            "rs",
        ),
        #[cfg(feature = "php")]
        (
            "<?php\nfunction f($a) { $x = 1; return $a + $x; }\n",
            Language::Php,
            "php",
        ),
        #[cfg(feature = "ruby")]
        ("def f(a)\n  x = 1\n  a + x\nend\n", Language::Ruby, "rb"),
        #[cfg(feature = "kotlin")]
        (
            "fun f(a: Int): Int { val x = 1; return a + x }\n",
            Language::Kotlin,
            "kt",
        ),
    ];

    let fixture_languages: HashSet<_> = fixtures.iter().map(|(_, language, _)| *language).collect();
    for language in available_languages() {
        let frontend = create_frontend(language).expect("available frontend must be constructible");
        if frontend.dataflow.capability().is_supported() {
            assert!(
                fixture_languages.contains(&language),
                "missing dataflow smoke fixture for {}",
                language.as_str()
            );
        }
    }

    for &(source, lang, ext) in fixtures {
        let Some(frontend) = create_frontend(lang) else {
            continue;
        };
        if !frontend.dataflow.capability().is_supported() {
            continue;
        }
        let file_id = FileId::generate(&format!("smoke.{ext}"));
        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            std::path::Path::new(&format!("smoke.{ext}")),
            source,
            "t",
            ExtractionMode::Full,
            &(),
        )
        .unwrap_or_else(|e| panic!("{lang:?} extraction failed: {e}"));

        let node_count = facts.data_nodes.len();
        let edge_count = facts.dataflow_edges.len();
        assert!(
            node_count > 0,
            "{lang:?} must produce at least 1 DataNode, got 0"
        );
        assert!(
            edge_count > 0,
            "{lang:?} must produce at least 1 DataFlowEdge, got 0 (nodes={node_count})"
        );

        // If lexical is supported, must produce at least some bindings
        if frontend.lexical.capability().is_supported() {
            let binding_count = facts.bindings.len();
            assert!(
                binding_count > 0,
                "{lang:?} lexical must produce at least 1 BindingDef, got 0"
            );
        }
    }
}
