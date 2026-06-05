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
use types::enums::Language;
use types::ids::{DataNodeId, FileId};
use types::structs::{ImportDef, ReferenceUse, ScopeDef, SymbolDef};

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
    /// Defaults to [`definition_query`] for languages that don't have a
    /// separate manifest query yet.
    fn manifest_query(&self) -> &str {
        self.definition_query()
    }
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
// Recovery spec
// ---------------------------------------------------------------------------

/// Recovery hook for languages that need to repair tree-sitter parse artifacts.
///
/// Called BEFORE manifest early-return and BEFORE scope_tree building.
/// Uses source text + AST hybrid to recover definitions and scopes
/// that tree-sitter cannot parse natively (e.g., ArkTS `struct`).
pub trait RecoverySpec: Send + Sync {
    /// Recover definitions that tree-sitter missed (e.g., ERROR node wrappers).
    ///
    /// Called after `extract_definition_symbols()`.
    /// Receives mutable access to symbols list so recovered definitions
    /// appear in both Manifest and Structural modes.
    fn recover_definitions(
        &self,
        source: &str,
        tree: &tree_sitter::Tree,
        file_id: FileId,
        symbols: &mut Vec<SymbolDef>,
        scopes: &mut Vec<ScopeDef>,
    );

    /// Recover scopes that tree-sitter missed.
    ///
    /// Called after `scope_extractor.extract_scopes()` but before
    /// `build_scope_tree()`.  Receives mutable access to both collections
    /// because scope recovery may need to recover container-enclosed
    /// members (e.g., struct methods).
    fn recover_scopes(
        &self,
        source: &str,
        tree: &tree_sitter::Tree,
        file_id: FileId,
        symbols: &mut Vec<SymbolDef>,
        scopes: &mut Vec<ScopeDef>,
    );
}

/// Default no-op recovery — does nothing.
pub struct NoOpRecovery;

impl RecoverySpec for NoOpRecovery {
    fn recover_definitions(
        &self,
        _source: &str,
        _tree: &tree_sitter::Tree,
        _file_id: FileId,
        _symbols: &mut Vec<SymbolDef>,
        _scopes: &mut Vec<ScopeDef>,
    ) {
    }
    fn recover_scopes(
        &self,
        _source: &str,
        _tree: &tree_sitter::Tree,
        _file_id: FileId,
        _symbols: &mut Vec<SymbolDef>,
        _scopes: &mut Vec<ScopeDef>,
    ) {
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
    pub recovery: Box<dyn RecoverySpec>,
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
    /// Parse artifact recovery (e.g., ArkTS `struct` via ERROR nodes).
    pub recovery: Box<dyn RecoverySpec>,
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
            recovery: parts.recovery,
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
    /// method returns it directly (cloned).  Only if the profile does not
    /// carry a `features` matrix (legacy/migration path) does it fall back
    /// to deriving a matrix from the slot capabilities.
    pub fn feature_matrix(&self) -> FeatureMatrix {
        if let Some(ref features) = self.capability.features {
            return features.clone();
        }
        // Fallback for legacy profiles without a typed feature matrix.
        FeatureMatrix {
            symbols: self.symbols.capability(),
            references: self.references.capability(),
            imports: self.imports.capability(),
            scopes: self.scopes.capability(),
            call_graph: FeatureSupport::supported_with_confidence(self.capability.confidence_floor),
            lexical_bindings: self.lexical.capability(),
            local_dataflow: self.dataflow.capability(),
            use_def: if self.lexical.capability().is_supported()
                && self.dataflow.capability().is_supported()
            {
                FeatureSupport::supported_with_limitations(
                    self.capability.confidence_floor,
                    vec!["name-based binding (no proper shadowing)"],
                )
            } else if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_limitations(
                    self.capability.confidence_floor,
                    vec![
                        "no lexical binding extraction",
                        "name-based use-def (may conflate same-named variables)",
                    ],
                )
            } else {
                FeatureSupport::unsupported("requires lexical bindings and dataflow")
            },
            field_access: if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("requires dataflow")
            },
            call_arguments: if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("requires dataflow")
            },
            returns_flow: if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("requires dataflow")
            },
            cfg: if self
                .capability
                .supported_features
                .contains(&"cfg".to_string())
            {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("CFG builder not available")
            },
            interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
        }
    }

    /// Derive a complete [`LanguageCapabilityProfile`] from the slot implementations.
    ///
    /// Unlike the hand-written profiles in `types::capability::profiles`,
    /// this method computes the profile directly from slot trait capabilities,
    /// guaranteeing that the profile always matches the actual implementation.
    pub fn derive_capability_profile(&self) -> LanguageCapabilityProfile {
        let fm = self.feature_matrix_from_slots();
        LanguageCapabilityProfile {
            language: self.language().as_str().into(),
            capability_level: fm.derive_capability_level(),
            supported_features: fm.supported_feature_names(),
            unsupported_features: fm.unsupported_feature_names(),
            limitations: self.capability.limitations.clone(),
            confidence_floor: fm.min_confidence_floor(),
            features: Some(fm),
        }
    }

    /// Build a [`FeatureMatrix`] exclusively from slot capabilities,
    /// ignoring any static `capability.features` cache.
    fn feature_matrix_from_slots(&self) -> FeatureMatrix {
        FeatureMatrix {
            symbols: self.symbols.capability(),
            references: self.references.capability(),
            imports: self.imports.capability(),
            scopes: self.scopes.capability(),
            call_graph: FeatureSupport::supported_with_confidence(self.capability.confidence_floor),
            lexical_bindings: self.lexical.capability(),
            local_dataflow: self.dataflow.capability(),
            use_def: if self.lexical.capability().is_supported()
                && self.dataflow.capability().is_supported()
            {
                FeatureSupport::supported_with_limitations(
                    self.capability.confidence_floor,
                    vec!["name-based binding (no proper shadowing)"],
                )
            } else if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_limitations(
                    self.capability.confidence_floor,
                    vec![
                        "no lexical binding extraction",
                        "name-based use-def (may conflate same-named variables)",
                    ],
                )
            } else {
                FeatureSupport::unsupported("requires lexical bindings and dataflow")
            },
            field_access: if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("requires dataflow")
            },
            call_arguments: if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("requires dataflow")
            },
            returns_flow: if self.dataflow.capability().is_supported() {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("requires dataflow")
            },
            cfg: if self
                .capability
                .supported_features
                .contains(&"cfg".to_string())
            {
                FeatureSupport::supported_with_confidence(self.capability.confidence_floor)
            } else {
                FeatureSupport::unsupported("CFG builder not available")
            },
            interprocedural_summaries: self
                .capability
                .features
                .as_ref()
                .map(|fm| fm.interprocedural_summaries.clone())
                .unwrap_or_else(|| FeatureSupport::unsupported("not implemented")),
        }
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
            "Java dataflow should be supported (DataflowBasic)"
        );
        assert!(
            frontend.lexical.capability().is_supported(),
            "Java lexical should be supported (DataflowBasic)"
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
    #[allow(clippy::vec_init_then_push)]
    fn test_capability_profile_matches_slot_capabilities() {
        let mut languages: Vec<Language> = Vec::new();
        #[cfg(feature = "typescript")]
        languages.push(Language::TypeScript);
        #[cfg(feature = "javascript")]
        languages.push(Language::JavaScript);
        #[cfg(feature = "python")]
        languages.push(Language::Python);
        #[cfg(feature = "java")]
        languages.push(Language::Java);
        #[cfg(feature = "c")]
        languages.push(Language::C);
        #[cfg(feature = "cpp")]
        languages.push(Language::Cpp);
        #[cfg(feature = "arkts")]
        languages.push(Language::ArkTS);
        #[cfg(feature = "cangjie")]
        languages.push(Language::Cangjie);

        assert!(
            !languages.is_empty(),
            "expected at least one language feature enabled"
        );

        for lang in languages {
            let frontend = crate::languages::create_frontend(lang)
                .unwrap_or_else(|| panic!("create_frontend failed for {lang:?}"));

            let profile = frontend.capability;
            let fm = profile
                .features
                .as_ref()
                .expect("profile must have FeatureMatrix");

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

    /// B5: derived capability profile must be AT LEAST as capable as static.
    ///
    /// The derived profile comes from slot trait implementations (ground truth).
    /// The static profile is a documentation snapshot that may lag behind.
    /// A feature that the derived profile claims as supported MUST also be
    /// supported in the static profile; the reverse is NOT required
    /// (static may claim supported features that the slots haven't caught up to).
    #[test]
    #[allow(clippy::vec_init_then_push)]
    fn test_auto_derived_profile_matches_static() {
        let mut languages: Vec<Language> = Vec::new();
        #[cfg(feature = "typescript")]
        languages.push(Language::TypeScript);
        #[cfg(feature = "javascript")]
        languages.push(Language::JavaScript);
        #[cfg(feature = "python")]
        languages.push(Language::Python);
        #[cfg(feature = "java")]
        languages.push(Language::Java);
        #[cfg(feature = "c")]
        languages.push(Language::C);
        #[cfg(feature = "cpp")]
        languages.push(Language::Cpp);
        #[cfg(feature = "arkts")]
        languages.push(Language::ArkTS);
        #[cfg(feature = "go")]
        languages.push(Language::Go);
        #[cfg(feature = "csharp")]
        languages.push(Language::CSharp);
        #[cfg(feature = "rust")]
        languages.push(Language::Rust);

        assert!(
            !languages.is_empty(),
            "expected at least one language enabled"
        );

        for lang in languages {
            let frontend = crate::languages::create_frontend(lang)
                .unwrap_or_else(|| panic!("create_frontend failed for {lang:?}"));

            let derived = frontend.derive_capability_profile();
            let static_profile = frontend.capability.clone();

            // Capability level must match
            assert_eq!(
                derived.capability_level, static_profile.capability_level,
                "{:?}: derived level {:?} != static level {:?}",
                lang, derived.capability_level, static_profile.capability_level
            );

            // Both must have FeatureMatrix
            let df = derived
                .features
                .as_ref()
                .expect("derived must have FeatureMatrix");
            let sf = static_profile
                .features
                .as_ref()
                .expect("static must have FeatureMatrix");

            // Derived profile should NOT under-report capability vs static.
            // Static may be outdated (claim unsupported when adapter actually implements it);
            // but if static claims supported, derived must also support it.
            if sf.symbols.is_supported() {
                assert!(
                    df.symbols.is_supported(),
                    "{lang:?}: derived under-reports symbols"
                );
            }
            if sf.local_dataflow.is_supported() {
                assert!(
                    df.local_dataflow.is_supported(),
                    "{lang:?}: derived under-reports dataflow"
                );
            }
            if sf.lexical_bindings.is_supported() {
                assert!(
                    df.lexical_bindings.is_supported(),
                    "{lang:?}: derived under-reports lexical"
                );
            }
            if sf.scopes.is_supported() {
                assert!(
                    df.scopes.is_supported(),
                    "{lang:?}: derived under-reports scopes"
                );
            }

            // Derived profile must produce valid string lists.
            // supported_features must never be empty for a working language.
            assert!(
                !derived.supported_features.is_empty(),
                "{lang:?}: no supported features"
            );
            // unsupported_features may be empty if all FeatureMatrix
            // capabilities report `is_supported()` — that is valid.
        }
    }
}

/// Verify that every language with lexical support has a compilable lexical query.
#[test]
fn test_all_lexical_queries_compile() {
    use crate::languages::create_frontend;
    use types::enums::Language;

    let languages_with_lexical = [
        #[cfg(feature = "typescript")]
        Language::TypeScript,
        #[cfg(feature = "javascript")]
        Language::JavaScript,
        #[cfg(feature = "python")]
        Language::Python,
        #[cfg(feature = "java")]
        Language::Java,
        #[cfg(feature = "c")]
        Language::C,
        #[cfg(feature = "cpp")]
        Language::Cpp,
        #[cfg(feature = "arkts")]
        Language::ArkTS,
        #[cfg(feature = "go")]
        Language::Go,
        #[cfg(feature = "csharp")]
        Language::CSharp,
        #[cfg(feature = "rust")]
        Language::Rust,
        #[cfg(feature = "php")]
        Language::Php,
        #[cfg(feature = "ruby")]
        Language::Ruby,
        #[cfg(feature = "kotlin")]
        Language::Kotlin,
    ];

    for &lang in &languages_with_lexical {
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
    use crate::extract::extract_file;
    use crate::languages::create_frontend;
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

    for &(source, lang, ext) in fixtures {
        let Some(frontend) = create_frontend(lang) else {
            continue;
        };
        if !frontend.dataflow.capability().is_supported() {
            continue;
        }
        let file_id = FileId::generate(&format!("smoke.{ext}"));
        let facts = extract_file(
            &frontend,
            file_id,
            std::path::Path::new(&format!("smoke.{ext}")),
            source,
            "t",
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
