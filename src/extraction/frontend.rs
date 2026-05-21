//! LanguageFrontend — slot-based language frontend replacing the monolithic
//! `LanguageAdapter` trait.
//!
//! ## Motivation
//!
//! The old `LanguageAdapter` trait has 17 methods: identity (3), queries (6),
//! normalize (6), hooks (2). Default impls return `""` / `None` / `Vec::new()`
//! for unsupported features, making it impossible for consumers to distinguish
//! "not supported" from "supported but no matches".
//!
//! ## Design
//!
//! `LanguageFrontend` is a struct with typed slot fields. Each slot is a
//! trait object (`Box<dyn SomeSpec>`) with a `capability()` method that
//! returns `FeatureSupport` instead of silently returning empty data.
//!
//! Unsupported slots are filled with typed `Unsupported*Spec` structs that
//! return `FeatureSupport::Unsupported(reason)`.
//!
//! ## Migration
//!
//! `LanguageFrontend` wraps the old `LanguageAdapter` for now. Slots are
//! populated one at a time; the adapter's query/normalize methods are used
//! as the fallback until all slots are migrated. This allows incremental
//! migration without breaking existing functionality.

use crate::extraction::callsite_spec::CallsiteExtractorSpec;
use crate::types::capability::{FeatureMatrix, FeatureSupport, LanguageCapabilityProfile};
use crate::types::enums::Language;

use super::languages::LanguageAdapter;

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
    /// Feature support for symbol extraction.
    fn capability(&self) -> FeatureSupport;
}

/// Reference extraction spec: tree-sitter query + normalization.
pub trait ReferenceExtractorSpec: Send + Sync {
    /// S-expression query for reference uses.
    fn reference_query(&self) -> &str;
    /// Feature support for reference extraction.
    fn capability(&self) -> FeatureSupport;
}

/// Import extraction spec: tree-sitter query + normalization.
pub trait ImportExtractorSpec: Send + Sync {
    /// S-expression query for import statements.
    fn import_query(&self) -> &str;
    /// Feature support for import extraction.
    fn capability(&self) -> FeatureSupport;
}

/// Scope extraction spec: tree-sitter query + normalization.
pub trait ScopeExtractorSpec: Send + Sync {
    /// S-expression query for scopes.
    fn scope_query(&self) -> &str;
    /// Feature support for scope extraction.
    fn capability(&self) -> FeatureSupport;
}

/// Lexical binding extraction spec.
pub trait LexicalBindingSpec: Send + Sync {
    /// S-expression query for lexical bindings.
    fn lexical_query(&self) -> &str;
    /// Feature support for lexical binding extraction.
    fn capability(&self) -> FeatureSupport;
}

/// Dataflow extraction spec.
pub trait DataflowSpec: Send + Sync {
    /// S-expression query for dataflow builder.
    fn dataflow_builder_query(&self) -> &str;
    /// Feature support for dataflow extraction.
    fn capability(&self) -> FeatureSupport;
}

// ---------------------------------------------------------------------------
// Unsupported spec stubs
// ---------------------------------------------------------------------------

/// Generic unsupported spec that returns `FeatureSupport::Unsupported`.
pub struct UnsupportedSpec {
    reason: String,
}

impl UnsupportedSpec {
    pub fn new(reason: &str) -> Self {
        Self {
            reason: reason.to_string(),
        }
    }
}

impl LexicalBindingSpec for UnsupportedSpec {
    fn lexical_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported(&self.reason)
    }
}

impl DataflowSpec for UnsupportedSpec {
    fn dataflow_builder_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported(&self.reason)
    }
}

impl ScopeExtractorSpec for UnsupportedSpec {
    fn scope_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported(&self.reason)
    }
}

// ---------------------------------------------------------------------------
// Adapter-backed spec implementations
// ---------------------------------------------------------------------------

/// Parser spec backed by a `LanguageAdapter`.
pub struct AdapterParserSpec {
    language: Language,
    ts_lang: tree_sitter::Language,
}

impl AdapterParserSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        Self {
            language: adapter.language(),
            ts_lang: adapter.tree_sitter_language(),
        }
    }
}

impl ParserSpec for AdapterParserSpec {
    fn language(&self) -> Language {
        self.language
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        self.ts_lang.clone()
    }
}

/// Symbol spec backed by a `LanguageAdapter`.
pub struct AdapterSymbolSpec {
    query: String,
    cap: FeatureSupport,
}

impl AdapterSymbolSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        Self {
            query: adapter.definition_query().to_string(),
            cap: FeatureSupport::supported(),
        }
    }
}

impl SymbolExtractorSpec for AdapterSymbolSpec {
    fn definition_query(&self) -> &str {
        &self.query
    }
    fn capability(&self) -> FeatureSupport {
        self.cap.clone()
    }
}

/// Reference spec backed by a `LanguageAdapter`.
pub struct AdapterReferenceSpec {
    query: String,
    cap: FeatureSupport,
}

impl AdapterReferenceSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        Self {
            query: adapter.reference_query().to_string(),
            cap: FeatureSupport::supported(),
        }
    }
}

impl ReferenceExtractorSpec for AdapterReferenceSpec {
    fn reference_query(&self) -> &str {
        &self.query
    }
    fn capability(&self) -> FeatureSupport {
        self.cap.clone()
    }
}

/// Import spec backed by a `LanguageAdapter`.
pub struct AdapterImportSpec {
    query: String,
    cap: FeatureSupport,
}

impl AdapterImportSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        Self {
            query: adapter.import_query().to_string(),
            cap: FeatureSupport::supported(),
        }
    }
}

impl ImportExtractorSpec for AdapterImportSpec {
    fn import_query(&self) -> &str {
        &self.query
    }
    fn capability(&self) -> FeatureSupport {
        self.cap.clone()
    }
}

/// Scope spec backed by a `LanguageAdapter`.
pub struct AdapterScopeSpec {
    query: String,
    cap: FeatureSupport,
}

impl AdapterScopeSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        let query = adapter.scope_query().to_string();
        let cap = if query.is_empty() {
            FeatureSupport::unsupported("scope query not provided by adapter")
        } else {
            FeatureSupport::supported()
        };
        Self { query, cap }
    }
}

impl ScopeExtractorSpec for AdapterScopeSpec {
    fn scope_query(&self) -> &str {
        &self.query
    }
    fn capability(&self) -> FeatureSupport {
        self.cap.clone()
    }
}

/// Lexical spec backed by a `LanguageAdapter`.
pub struct AdapterLexicalSpec {
    query: String,
    cap: FeatureSupport,
}

impl AdapterLexicalSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        let query = adapter.lexical_query().to_string();
        let cap = if query.is_empty() {
            FeatureSupport::unsupported("lexical query not provided by adapter")
        } else {
            FeatureSupport::supported_with_limitations(
                0.55,
                vec!["name-based binding (no proper shadowing)"],
            )
        };
        Self { query, cap }
    }
}

impl LexicalBindingSpec for AdapterLexicalSpec {
    fn lexical_query(&self) -> &str {
        &self.query
    }
    fn capability(&self) -> FeatureSupport {
        self.cap.clone()
    }
}

/// Dataflow spec backed by a `LanguageAdapter`.
pub struct AdapterDataflowSpec {
    query: String,
    cap: FeatureSupport,
}

impl AdapterDataflowSpec {
    pub fn from_adapter(adapter: &dyn LanguageAdapter) -> Self {
        let query = adapter.dataflow_builder_query().to_string();
        let cap = if query.is_empty() {
            FeatureSupport::unsupported("dataflow query not provided by adapter")
        } else {
            FeatureSupport::supported_with_limitations(
                0.55,
                vec!["capture-order assignment pairing (Nth target ≈ Nth expr)"],
            )
        };
        Self { query, cap }
    }
}

impl DataflowSpec for AdapterDataflowSpec {
    fn dataflow_builder_query(&self) -> &str {
        &self.query
    }
    fn capability(&self) -> FeatureSupport {
        self.cap.clone()
    }
}

// ---------------------------------------------------------------------------
// LanguageFrontend
// ---------------------------------------------------------------------------

/// Slot-based language frontend replacing the monolithic `LanguageAdapter`.
///
/// Each slot is a trait object with a `capability()` method, enabling
/// type-safe feature queries instead of string-contains probes.
///
/// ## Construction
///
/// Use [`LanguageFrontend::from_adapter`] to wrap an existing `LanguageAdapter`
/// (migration path), or construct directly with typed slots.
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
    pub dataflow: Box<dyn DataflowSpec>,
    /// Language capability profile (used by TraceEngine for gating).
    pub capability: LanguageCapabilityProfile,
    /// Backward-compatible adapter reference (used for normalize_* methods
    /// until those are migrated to spec traits).
    adapter: Box<dyn LanguageAdapter>,
}

impl LanguageFrontend {
    /// Create a `LanguageFrontend` wrapping an existing `LanguageAdapter`.
    ///
    /// This is the migration path: the adapter is used for `normalize_*`
    /// methods, while the slot-based specs provide typed feature queries.
    pub fn from_adapter(adapter: Box<dyn LanguageAdapter>) -> Self {
        let lang = adapter.language();
        let cap = LanguageCapabilityProfile::for_language(lang);

        // Build callsite extractor from language
        let callsite_extractor = super::callsite_spec::create_extractor(lang);

        Self {
            parser: Box::new(AdapterParserSpec::from_adapter(adapter.as_ref())),
            symbols: Box::new(AdapterSymbolSpec::from_adapter(adapter.as_ref())),
            references: Box::new(AdapterReferenceSpec::from_adapter(adapter.as_ref())),
            imports: Box::new(AdapterImportSpec::from_adapter(adapter.as_ref())),
            scopes: Box::new(AdapterScopeSpec::from_adapter(adapter.as_ref())),
            callsites: callsite_extractor,
            lexical: Box::new(AdapterLexicalSpec::from_adapter(adapter.as_ref())),
            dataflow: Box::new(AdapterDataflowSpec::from_adapter(adapter.as_ref())),
            capability: cap,
            adapter,
        }
    }

    /// Access the underlying adapter for `normalize_*` calls.
    ///
    /// This exists for the migration period. Once normalize methods are
    /// moved to spec traits, this accessor will be removed.
    pub fn adapter(&self) -> &dyn LanguageAdapter {
        self.adapter.as_ref()
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
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "typescript")]
    #[test]
    fn test_frontend_from_adapter_ts() {
        let adapter = crate::extraction::languages::create_adapter(Language::TypeScript)
            .expect("TS adapter should exist");
        let frontend = LanguageFrontend::from_adapter(adapter);

        assert_eq!(frontend.language(), Language::TypeScript);
        assert!(frontend.symbols.capability().is_supported());
        assert!(frontend.references.capability().is_supported());
        assert!(frontend.dataflow.capability().is_supported());
        assert!(frontend.lexical.capability().is_supported());
    }

    #[cfg(feature = "python")]
    #[test]
    fn test_frontend_from_adapter_python() {
        let adapter = crate::extraction::languages::create_adapter(Language::Python)
            .expect("Python adapter should exist");
        let frontend = LanguageFrontend::from_adapter(adapter);

        assert_eq!(frontend.language(), Language::Python);
        assert!(frontend.dataflow.capability().is_supported());
        assert!(
            !frontend.lexical.capability().is_supported(),
            "Python lexical should be unsupported"
        );
    }

    #[cfg(feature = "java")]
    #[test]
    fn test_frontend_from_adapter_java() {
        let adapter = crate::extraction::languages::create_adapter(Language::Java)
            .expect("Java adapter should exist");
        let frontend = LanguageFrontend::from_adapter(adapter);

        assert_eq!(frontend.language(), Language::Java);
        assert!(
            !frontend.dataflow.capability().is_supported(),
            "Java dataflow should be unsupported"
        );
        assert!(
            !frontend.lexical.capability().is_supported(),
            "Java lexical should be unsupported"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_frontend_feature_matrix_ts() {
        let adapter = crate::extraction::languages::create_adapter(Language::TypeScript)
            .expect("TS adapter should exist");
        let frontend = LanguageFrontend::from_adapter(adapter);
        let matrix = frontend.feature_matrix();

        assert!(matrix.local_dataflow.is_supported());
        assert!(matrix.lexical_bindings.is_supported());
        assert!(!matrix.interprocedural_summaries.is_supported());
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_create_frontend_factory() {
        let frontend = crate::extraction::languages::create_frontend(Language::TypeScript)
            .expect("create_frontend should return TS frontend");
        assert_eq!(frontend.language(), Language::TypeScript);
    }
}
