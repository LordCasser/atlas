//! Language capability profiles: declare what analysis features each language
//! supports and the confidence level at which they are delivered.
//!
//! These profiles are consumed by `atlas status`, `atlas doctor`, MCP tools
//! (`atlas_status`, `atlas_language_capabilities`), and the trace layer to
//! set user expectations about accuracy and completeness.
//!
//! ## Feature-level granularity
//!
//! Each feature is represented as a [`FeatureSupport`] value, collected into a
//! [`FeatureMatrix`] on the profile.  This replaces the flat
//! `supported_features` / `unsupported_features` string lists, enabling
//! type-safe capability checks (e.g. `cap.features.local_dataflow.is_supported()`)
//! instead of string-contains probes.

use serde::{Deserialize, Serialize};

use crate::enums::Language;

// ---------------------------------------------------------------------------
// FeatureSupport
// ---------------------------------------------------------------------------

/// Per-feature support status with structured diagnostics.
///
/// Replaces the old convention of empty-string queries and
/// `supported_features`/`unsupported_features` string lists.
/// Consumers can distinguish "not supported" from "supported but no matches"
/// and surface the `reason` / `limitations` to AI agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FeatureSupport {
    /// The feature is available for this language.
    Supported {
        /// Floor confidence (0.0–1.0) for facts produced by this feature.
        /// Values below this threshold should be treated as best-effort.
        confidence_floor: f64,
        /// Known accuracy or completeness caveats.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        limitations: Vec<String>,
    },
    /// The feature is not available for this language.
    Unsupported {
        /// Human-readable reason (e.g. "no DataFlowBuilder for Python").
        reason: String,
    },
}

impl FeatureSupport {
    /// Convenience: supported with no limitations and default confidence.
    pub fn supported() -> Self {
        Self::Supported {
            confidence_floor: 0.5,
            limitations: vec![],
        }
    }

    /// Convenience: supported with explicit confidence floor.
    pub fn supported_with_confidence(floor: f64) -> Self {
        Self::Supported {
            confidence_floor: floor,
            limitations: vec![],
        }
    }

    /// Convenience: supported with limitations.
    pub fn supported_with_limitations(floor: f64, limitations: Vec<&'static str>) -> Self {
        Self::Supported {
            confidence_floor: floor,
            limitations: limitations.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Convenience: unsupported with a reason.
    pub fn unsupported(reason: &str) -> Self {
        Self::Unsupported {
            reason: reason.to_string(),
        }
    }

    /// Returns `true` if this is `Supported`.
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }

    /// Returns the confidence floor if supported, else `None`.
    pub fn confidence_floor(&self) -> Option<f64> {
        match self {
            Self::Supported {
                confidence_floor, ..
            } => Some(*confidence_floor),
            Self::Unsupported { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// FeatureMatrix
// ---------------------------------------------------------------------------

/// Typed feature matrix replacing flat `supported_features` / `unsupported_features`.
///
/// Each field is a [`FeatureSupport`] that can be queried directly:
/// ```ignore
/// if profile.features.local_dataflow.is_supported() {
///     // run dataflow trace
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureMatrix {
    /// Symbol definitions (functions, classes, variables, etc.).
    pub symbols: FeatureSupport,
    /// Reference uses (call sites, type references, etc.).
    pub references: FeatureSupport,
    /// Import / include resolution.
    pub imports: FeatureSupport,
    /// Scope extraction (function bodies, block scopes).
    pub scopes: FeatureSupport,
    /// Call graph edges (caller→callee).
    pub call_graph: FeatureSupport,
    /// Lexical binding extraction (let/const/var, parameters, destructuring).
    pub lexical_bindings: FeatureSupport,
    /// Intra-function dataflow (assignments, call arguments, member access).
    pub local_dataflow: FeatureSupport,
    /// Use-def chains (variable → definition, argument → parameter).
    pub use_def: FeatureSupport,
    /// Field / property access chains (obj.field.nested).
    pub field_access: FeatureSupport,
    /// Call argument extraction (argument positions within call expressions).
    pub call_arguments: FeatureSupport,
    /// Return value flow (return statements → caller).
    #[serde(rename = "returns")]
    pub returns_flow: FeatureSupport,
    /// Per-function control flow graph.
    pub cfg: FeatureSupport,
    /// Interprocedural dataflow summaries (caller arg → callee param).
    pub interprocedural_summaries: FeatureSupport,
}

impl FeatureMatrix {
    /// Returns the coarse [`CapabilityLevel`] derived from the matrix.
    ///
    /// This preserves backward compatibility with code that checks
    /// `level >= DataflowBasic`.
    pub fn derive_capability_level(&self) -> CapabilityLevel {
        if self.local_dataflow.is_supported() && self.use_def.is_supported() {
            CapabilityLevel::DataflowBasic
        } else if self.symbols.is_supported() && self.references.is_supported() {
            CapabilityLevel::Symbolic
        } else {
            CapabilityLevel::None
        }
    }

    /// Returns the minimum confidence floor across all supported features.
    pub fn min_confidence_floor(&self) -> f64 {
        let floors: Vec<f64> = [
            self.symbols.confidence_floor(),
            self.references.confidence_floor(),
            self.imports.confidence_floor(),
            self.scopes.confidence_floor(),
            self.call_graph.confidence_floor(),
            self.lexical_bindings.confidence_floor(),
            self.local_dataflow.confidence_floor(),
            self.use_def.confidence_floor(),
            self.field_access.confidence_floor(),
            self.call_arguments.confidence_floor(),
            self.returns_flow.confidence_floor(),
            self.cfg.confidence_floor(),
            self.interprocedural_summaries.confidence_floor(),
        ]
        .into_iter()
        .flatten()
        .collect();

        floors.into_iter().fold(1.0, f64::min)
    }

    /// Human-readable feature names that are currently supported.
    pub fn supported_feature_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.symbols.is_supported() { names.push("symbol_extraction".into()); }
        if self.references.is_supported() { names.push("reference_extraction".into()); }
        if self.imports.is_supported() { names.push("import_resolution".into()); }
        if self.scopes.is_supported() { names.push("scope_extraction".into()); }
        if self.call_graph.is_supported() { names.push("call_graph".into()); }
        if self.lexical_bindings.is_supported() { names.push("lexical_bindings".into()); }
        if self.local_dataflow.is_supported() { names.push("intra_statement_dataflow".into()); }
        if self.use_def.is_supported() { names.push("use_def_heuristic".into()); }
        if self.field_access.is_supported() { names.push("access_path".into()); }
        if self.call_arguments.is_supported() { names.push("call_arguments".into()); }
        if self.returns_flow.is_supported() { names.push("return_flow".into()); }
        if self.cfg.is_supported() { names.push("cfg".into()); }
        if self.interprocedural_summaries.is_supported() { names.push("interprocedural_dataflow".into()); }
        names
    }

    /// Human-readable feature names that are NOT currently supported.
    pub fn unsupported_feature_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if !self.symbols.is_supported() { names.push("symbol_extraction".into()); }
        if !self.references.is_supported() { names.push("reference_extraction".into()); }
        if !self.imports.is_supported() { names.push("import_resolution".into()); }
        if !self.scopes.is_supported() { names.push("scope_extraction".into()); }
        if !self.call_graph.is_supported() { names.push("call_graph".into()); }
        if !self.lexical_bindings.is_supported() { names.push("lexical_bindings".into()); }
        if !self.local_dataflow.is_supported() { names.push("intra_statement_dataflow".into()); }
        if !self.use_def.is_supported() { names.push("use_def_heuristic".into()); }
        if !self.field_access.is_supported() { names.push("access_path".into()); }
        if !self.call_arguments.is_supported() { names.push("call_arguments".into()); }
        if !self.returns_flow.is_supported() { names.push("return_flow".into()); }
        if !self.cfg.is_supported() { names.push("cfg".into()); }
        if !self.interprocedural_summaries.is_supported() { names.push("interprocedural_dataflow".into()); }
        names
    }
}

// ---------------------------------------------------------------------------
// CapabilityLevel
// ---------------------------------------------------------------------------

/// How far the analysis pipeline can go for a given language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    /// Extraction only — parse + symbol/reference/import extraction.
    /// No resolution guarantees, no dataflow.
    None,
    /// Symbol-level analysis only: symbol defs, references, import resolution,
    /// call-graph edges. No dataflow edges.
    Symbolic,
    /// Lexical bindings + intra-statement dataflow (heuristic name-based
    /// binding, capture-order assignment pairing). Use-def resolution exists
    /// but may miss shadowed variables or complex expression trees.
    DataflowBasic,
    /// Cross-statement use-def (scope-aware, shadowing-safe), backward trace
    /// with access-path chains, caller-path exploration, interprocedural flow.
    DataflowFull,
}

impl CapabilityLevel {
    /// Short human-readable string matching the serde variant name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Symbolic => "symbolic",
            Self::DataflowBasic => "dataflow_basic",
            Self::DataflowFull => "dataflow_full",
        }
    }

    /// Parse from a lower-case string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "symbolic" => Some(Self::Symbolic),
            "dataflow_basic" => Some(Self::DataflowBasic),
            "dataflow_full" => Some(Self::DataflowFull),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// LanguageCapabilityProfile
// ---------------------------------------------------------------------------

/// Immutable static description of what a language's extraction & analysis
/// pipeline can produce, plus known limitations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCapabilityProfile {
    /// Two-letter-ish language tag (e.g. "typescript").
    pub language: String,
    /// Current capability ceiling for this language.
    pub capability_level: CapabilityLevel,
    /// Capabilities that are delivered (feature names like "call_graph",
    /// "intra_statement_dataflow", "cfg").
    ///
    /// **Prefer using `features` for type-safe queries.** This field is
    /// retained for backward compatibility with MCP `atlas_status` and
    /// existing consumers.
    pub supported_features: Vec<String>,
    /// Capabilities that the pipeline cannot yet provide for this language.
    ///
    /// **Prefer using `features` for type-safe queries.**
    pub unsupported_features: Vec<String>,
    /// Known accuracy / completeness caveats.
    pub limitations: Vec<String>,
    /// Floor confidence value (0.0–1.0) for edges produced in this language.
    /// Consumers should treat edges below this threshold as best-effort.
    pub confidence_floor: f64,
    /// Typed per-feature support matrix.  Use this for capability-gated
    /// execution (e.g. `cap.features.local_dataflow.is_supported()`) instead
    /// of string-contains on `supported_features`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeatureMatrix>,
}

impl LanguageCapabilityProfile {
    /// Look up the static profile for every language compiled into the binary.
    pub fn for_language(lang: Language) -> Self {
        profiles::make(lang)
    }

    /// All profiles for the languages whose tree-sitter grammars are compiled in.
    pub fn all_compiled() -> Vec<Self> {
        use Language::*;
        let mut profiles = Vec::with_capacity(8);

        #[cfg(feature = "typescript")]
        {
            profiles.push(Self::for_language(TypeScript));
            profiles.push(Self::for_language(JavaScript));
        }
        #[cfg(feature = "python")]
        profiles.push(Self::for_language(Python));
        #[cfg(feature = "java")]
        profiles.push(Self::for_language(Java));
        #[cfg(feature = "c")]
        profiles.push(Self::for_language(C));
        #[cfg(feature = "cpp")]
        profiles.push(Self::for_language(Cpp));
        #[cfg(feature = "arkts")]
        profiles.push(Self::for_language(ArkTS));
        #[cfg(feature = "cangjie")]
        profiles.push(Self::for_language(Cangjie));
        #[cfg(feature = "go")]
        profiles.push(Self::for_language(Go));
        #[cfg(feature = "csharp")]
        profiles.push(Self::for_language(CSharp));
        #[cfg(feature = "rust")]
        profiles.push(Self::for_language(Rust));
        #[cfg(feature = "php")]
        profiles.push(Self::for_language(Php));
        #[cfg(feature = "ruby")]
        profiles.push(Self::for_language(Ruby));
        #[cfg(feature = "bash")]
        profiles.push(Self::for_language(Bash));
        #[cfg(feature = "kotlin")]
        profiles.push(Self::for_language(Kotlin));

        profiles
    }
}

// ---------------------------------------------------------------------------
// Static profile definitions
// ---------------------------------------------------------------------------

mod profiles {
    use super::*;

    pub fn make(lang: Language) -> LanguageCapabilityProfile {
        match lang {
            Language::TypeScript => ts_profile(),
            Language::JavaScript => js_profile(),
            Language::Python => py_profile(),
            Language::Java => java_profile(),
            Language::C => c_profile(),
            Language::Cpp => cpp_profile(),
            Language::ArkTS => arkts_profile(),
            Language::Cangjie => cangjie_profile(),
            Language::Go => go_profile(),
            Language::CSharp => csharp_profile(),
            Language::Rust => rust_profile(),
            Language::Php => php_profile(),
            Language::Ruby => ruby_profile(),
            Language::Bash => bash_profile(),
            Language::Kotlin => kotlin_profile(),
        }
    }

    // ---- TypeScript -------------------------------------------------------

    fn ts_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "typescript".into(),
            capability_level: CapabilityLevel::DataflowBasic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
                "lexical_bindings".into(),
                "intra_statement_dataflow".into(),
                "use_def_heuristic".into(),
                "access_path".into(),
                "cfg".into(),
            ],
            unsupported_features: vec![
                "scope_aware_binding".into(),
                "interprocedural_dataflow".into(),
            ],
            limitations: vec![
                "name-based binding (no proper shadowing)".into(),
                "capture-order assignment pairing (Nth target ≈ Nth expr)".into(),
                "ArgToParam edges are call_arg→call_target, not caller-arg→callee-param".into(),
            ],
            confidence_floor: 0.55,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.55),
                references: FeatureSupport::supported_with_confidence(0.55),
                imports: FeatureSupport::supported_with_confidence(0.55),
                scopes: FeatureSupport::supported_with_confidence(0.55),
                call_graph: FeatureSupport::supported_with_confidence(0.55),
                lexical_bindings: FeatureSupport::supported_with_limitations(
                    0.55,
                    vec!["name-based binding (no proper shadowing)"],
                ),
                local_dataflow: FeatureSupport::supported_with_limitations(
                    0.55,
                    vec!["capture-order assignment pairing (Nth target ≈ Nth expr)"],
                ),
                use_def: FeatureSupport::supported_with_limitations(
                    0.55,
                    vec!["name-based binding (no proper shadowing)"],
                ),
                field_access: FeatureSupport::supported_with_confidence(0.55),
                call_arguments: FeatureSupport::supported_with_confidence(0.55),
                returns_flow: FeatureSupport::supported_with_confidence(0.55),
                cfg: FeatureSupport::supported_with_confidence(0.55),
                interprocedural_summaries: FeatureSupport::unsupported(
                    "not implemented; ArgToParam edges are call_arg→call_target, not caller-arg→callee-param",
                ),
            }),
        }
    }

    // ---- JavaScript -------------------------------------------------------

    fn js_profile() -> LanguageCapabilityProfile {
        // JavaScript shares the TypeScript adapter; same capabilities/limits.
        LanguageCapabilityProfile {
            language: "javascript".into(),
            capability_level: CapabilityLevel::DataflowBasic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
                "lexical_bindings".into(),
                "intra_statement_dataflow".into(),
                "use_def_heuristic".into(),
                "access_path".into(),
                "cfg".into(),
            ],
            unsupported_features: vec![
                "scope_aware_binding".into(),
                "interprocedural_dataflow".into(),
            ],
            limitations: vec![
                "shares TypeScript adapter (TSX-only constructs may trigger warnings)".into(),
                "name-based binding (no proper shadowing)".into(),
                "capture-order assignment pairing (Nth target ≈ Nth expr)".into(),
            ],
            confidence_floor: 0.55,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.55),
                references: FeatureSupport::supported_with_confidence(0.55),
                imports: FeatureSupport::supported_with_confidence(0.55),
                scopes: FeatureSupport::supported_with_confidence(0.55),
                call_graph: FeatureSupport::supported_with_confidence(0.55),
                lexical_bindings: FeatureSupport::supported_with_limitations(
                    0.55,
                    vec!["name-based binding (no proper shadowing)"],
                ),
                local_dataflow: FeatureSupport::supported_with_limitations(
                    0.55,
                    vec!["capture-order assignment pairing (Nth target ≈ Nth expr)"],
                ),
                use_def: FeatureSupport::supported_with_limitations(
                    0.55,
                    vec!["name-based binding (no proper shadowing)"],
                ),
                field_access: FeatureSupport::supported_with_confidence(0.55),
                call_arguments: FeatureSupport::supported_with_confidence(0.55),
                returns_flow: FeatureSupport::supported_with_confidence(0.55),
                cfg: FeatureSupport::supported_with_confidence(0.55),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Python -----------------------------------------------------------

    fn py_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "python".into(),
            capability_level: CapabilityLevel::DataflowBasic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
                "intra_statement_dataflow".into(),
                "use_def_heuristic".into(),
                "access_path".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "scope_aware_binding".into(),
                "interprocedural_dataflow".into(),
                "cfg".into(),
            ],
            limitations: vec![
                "no lexical binding extraction (LexicalBinder not implemented for Python)".into(),
                "name-based use-def resolution (may conflate same-named variables)".into(),
                "capture-order assignment pairing (Nth target ≈ Nth expr)".into(),
            ],
            confidence_floor: 0.50,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.50),
                references: FeatureSupport::supported_with_confidence(0.50),
                imports: FeatureSupport::supported_with_confidence(0.50),
                scopes: FeatureSupport::unsupported("scope query not implemented for Python"),
                call_graph: FeatureSupport::supported_with_confidence(0.50),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Python",
                ),
                local_dataflow: FeatureSupport::supported_with_limitations(
                    0.50,
                    vec!["capture-order assignment pairing (Nth target ≈ Nth expr)"],
                ),
                use_def: FeatureSupport::supported_with_limitations(
                    0.50,
                    vec![
                        "name-based use-def (may conflate same-named variables)",
                        "no lexical binding extraction",
                    ],
                ),
                field_access: FeatureSupport::supported_with_confidence(0.50),
                call_arguments: FeatureSupport::supported_with_confidence(0.50),
                returns_flow: FeatureSupport::supported_with_confidence(0.50),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Python"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Java -------------------------------------------------------------

    fn java_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "java".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
            ],
            confidence_floor: 0.70,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.70),
                references: FeatureSupport::supported_with_confidence(0.70),
                imports: FeatureSupport::supported_with_confidence(0.70),
                scopes: FeatureSupport::unsupported("scope query not implemented for Java"),
                call_graph: FeatureSupport::supported_with_confidence(0.70),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Java",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Java",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Java)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Java)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Java)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Java)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Java"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- C ----------------------------------------------------------------

    fn c_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "c".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "include_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
            ],
            confidence_floor: 0.70,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.70),
                references: FeatureSupport::supported_with_confidence(0.70),
                imports: FeatureSupport::supported_with_confidence(0.70),
                scopes: FeatureSupport::unsupported("scope query not implemented for C"),
                call_graph: FeatureSupport::supported_with_confidence(0.70),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for C",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for C",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for C)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for C"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- C++ --------------------------------------------------------------

    fn cpp_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "cpp".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "include_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
            ],
            confidence_floor: 0.70,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.70),
                references: FeatureSupport::supported_with_confidence(0.70),
                imports: FeatureSupport::supported_with_confidence(0.70),
                scopes: FeatureSupport::unsupported("scope query not implemented for C++"),
                call_graph: FeatureSupport::supported_with_confidence(0.70),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for C++",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for C++",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for C++)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C++)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C++)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C++)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for C++"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- ArkTS ------------------------------------------------------------

    fn arkts_profile() -> LanguageCapabilityProfile {
        // ArkTS delegates to the TypeScript frontend for structural extraction.
        // Lexical bindings and dataflow are NOT supported by the delegate.
        LanguageCapabilityProfile {
            language: "arkts".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "scope_aware_binding".into(),
                "interprocedural_dataflow".into(),
                "lexical_bindings".into(),
                "intra_statement_dataflow".into(),
                "use_def_heuristic".into(),
                "access_path".into(),
                "cfg".into(),
            ],
            limitations: vec![
                "delegates to TypeScript frontend (ArkTS-specific constructs may be missed)".into(),
            ],
            confidence_floor: 0.50,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.50),
                references: FeatureSupport::supported_with_confidence(0.50),
                imports: FeatureSupport::supported_with_confidence(0.50),
                scopes: FeatureSupport::supported_with_confidence(0.50),
                call_graph: FeatureSupport::supported_with_confidence(0.50),
                lexical_bindings: FeatureSupport::unsupported(
                    "ArkTS does not support lexical binding extraction",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "ArkTS does not support dataflow extraction",
                ),
                use_def: FeatureSupport::unsupported("ArkTS does not support dataflow extraction"),
                field_access: FeatureSupport::unsupported(
                    "ArkTS does not support dataflow extraction",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "ArkTS does not support dataflow extraction",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "ArkTS does not support dataflow extraction",
                ),
                cfg: FeatureSupport::unsupported("ArkTS does not support dataflow extraction"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Cangjie ----------------------------------------------------------

    fn cangjie_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "cangjie".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
            ],
            unsupported_features: vec![
                "call_graph".into(),
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "call graph edges not produced for Cangjie in current adapter".into(),
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
            ],
            confidence_floor: 0.60,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.60),
                references: FeatureSupport::supported_with_confidence(0.60),
                imports: FeatureSupport::supported_with_confidence(0.60),
                scopes: FeatureSupport::unsupported("scope query not implemented for Cangjie"),
                call_graph: FeatureSupport::unsupported(
                    "call graph edges not produced for Cangjie in current adapter",
                ),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Cangjie",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Cangjie",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Cangjie)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Cangjie)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Cangjie)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Cangjie)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Cangjie"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Go ----------------------------------------------------------------

    fn go_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "go".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "generic type parameters not captured in Symbolic level".into(),
                "interface embedding treated as type reference".into(),
            ],
            confidence_floor: 0.75,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.75),
                references: FeatureSupport::supported_with_confidence(0.75),
                imports: FeatureSupport::supported_with_confidence(0.75),
                scopes: FeatureSupport::supported_with_confidence(0.75),
                call_graph: FeatureSupport::supported_with_confidence(0.75),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Go",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Go",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Go)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Go)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Go)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Go)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Go"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- C# ----------------------------------------------------------------

    fn csharp_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "csharp".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "delegate/event declarations simplified (delegate→Function, event skipped)".into(),
                "partial classes across files not merged".into(),
                "record/record struct treated as class/struct".into(),
            ],
            confidence_floor: 0.75,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.75),
                references: FeatureSupport::supported_with_confidence(0.75),
                imports: FeatureSupport::supported_with_confidence(0.75),
                scopes: FeatureSupport::supported_with_confidence(0.75),
                call_graph: FeatureSupport::supported_with_confidence(0.75),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for C#",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for C#",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for C#)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C#)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C#)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for C#)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for C#"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Rust --------------------------------------------------------------

    fn rust_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "rust".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "lifetime parameters and where clauses not captured".into(),
                "macro_rules! body patterns not analyzed".into(),
                "impl trait / dyn trait treated as simple type reference".into(),
            ],
            confidence_floor: 0.70,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.70),
                references: FeatureSupport::supported_with_confidence(0.70),
                imports: FeatureSupport::supported_with_confidence(0.70),
                scopes: FeatureSupport::supported_with_confidence(0.70),
                call_graph: FeatureSupport::supported_with_confidence(0.70),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Rust",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Rust",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Rust)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Rust)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Rust)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Rust)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Rust"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- PHP ---------------------------------------------------------------

    fn php_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "php".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "anonymous functions / closures named `<closure>`".into(),
                "dynamic method calls via variable not resolved".into(),
                "namespace aliases resolved at reference resolution layer".into(),
            ],
            confidence_floor: 0.65,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.65),
                references: FeatureSupport::supported_with_confidence(0.65),
                imports: FeatureSupport::supported_with_confidence(0.65),
                scopes: FeatureSupport::supported_with_confidence(0.65),
                call_graph: FeatureSupport::supported_with_confidence(0.65),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for PHP",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for PHP",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for PHP)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for PHP)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for PHP)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for PHP)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for PHP"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Ruby --------------------------------------------------------------

    fn ruby_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "ruby".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "method_missing / define_method dynamic methods not captured".into(),
                "include/extend/prepend mixin methods not expanded".into(),
                "block/yield implicit calls not tracked".into(),
            ],
            confidence_floor: 0.55,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.55),
                references: FeatureSupport::supported_with_confidence(0.55),
                imports: FeatureSupport::supported_with_confidence(0.55),
                scopes: FeatureSupport::supported_with_confidence(0.55),
                call_graph: FeatureSupport::supported_with_confidence(0.55),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Ruby",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Ruby",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Ruby)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Ruby)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Ruby)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Ruby)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Ruby"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Bash (opt-in-only, scripting language) ----------------------------

    fn bash_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "bash".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "import_resolution".into(),
                "scope_extraction".into(),
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "scopes limited to file and function (no block scoping)".into(),
                "command call targets may be variables/string-interpolated — unresolvable statically".into(),
                "source / . builtins produce best-effort import facts".into(),
                "alias and positional parameters ($1, $@) not captured".into(),
            ],
            confidence_floor: 0.40,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.50),
                references: FeatureSupport::supported_with_confidence(0.50),
                imports: FeatureSupport::unsupported(
                    "import/include mapping unreliable for Bash source builtins",
                ),
                scopes: FeatureSupport::unsupported("scope query not implemented for Bash"),
                call_graph: FeatureSupport::supported_with_limitations(
                    0.40,
                    vec!["command calls may be dynamically resolved — low confidence"],
                ),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Bash",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Bash",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Bash)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Bash)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Bash)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Bash)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Bash"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }

    // ---- Kotlin ------------------------------------------------------------

    fn kotlin_profile() -> LanguageCapabilityProfile {
        LanguageCapabilityProfile {
            language: "kotlin".into(),
            capability_level: CapabilityLevel::Symbolic,
            supported_features: vec![
                "symbol_extraction".into(),
                "reference_extraction".into(),
                "import_resolution".into(),
                "call_graph".into(),
            ],
            unsupported_features: vec![
                "lexical_bindings".into(),
                "dataflow".into(),
                "cfg".into(),
                "backward_trace".into(),
            ],
            limitations: vec![
                "no DataFlowBuilder (dataflow queries not implemented)".into(),
                "no LexicalBinder (lexical queries not implemented)".into(),
                "extension functions not differentiated from regular functions".into(),
                "object declarations treated as classes".into(),
                "companion objects not separately modeled".into(),
            ],
            confidence_floor: 0.70,
            features: Some(FeatureMatrix {
                symbols: FeatureSupport::supported_with_confidence(0.70),
                references: FeatureSupport::supported_with_confidence(0.70),
                imports: FeatureSupport::supported_with_confidence(0.70),
                scopes: FeatureSupport::supported_with_confidence(0.70),
                call_graph: FeatureSupport::supported_with_confidence(0.70),
                lexical_bindings: FeatureSupport::unsupported(
                    "LexicalBinder not implemented for Kotlin",
                ),
                local_dataflow: FeatureSupport::unsupported(
                    "DataFlowBuilder not implemented for Kotlin",
                ),
                use_def: FeatureSupport::unsupported(
                    "requires lexical bindings and dataflow (both not implemented for Kotlin)",
                ),
                field_access: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Kotlin)",
                ),
                call_arguments: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Kotlin)",
                ),
                returns_flow: FeatureSupport::unsupported(
                    "requires dataflow (not implemented for Kotlin)",
                ),
                cfg: FeatureSupport::unsupported("CFG builder not implemented for Kotlin"),
                interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_level_roundtrip() {
        for level in &[
            CapabilityLevel::None,
            CapabilityLevel::Symbolic,
            CapabilityLevel::DataflowBasic,
            CapabilityLevel::DataflowFull,
        ] {
            let s = level.as_str();
            let parsed = CapabilityLevel::from_str(s);
            assert_eq!(parsed, Some(*level), "roundtrip failed for {:?}", level);
        }
    }

    #[test]
    fn test_ts_profile_exists() {
        let p = LanguageCapabilityProfile::for_language(Language::TypeScript);
        assert_eq!(p.language, "typescript");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowBasic);
        assert!(!p.supported_features.is_empty());
        assert!(!p.limitations.is_empty());
    }

    #[test]
    fn test_all_profiles_are_valid() {
        let profiles = LanguageCapabilityProfile::all_compiled();
        assert!(
            !profiles.is_empty(),
            "should have at least one compiled language"
        );
        for p in &profiles {
            assert!(
                !p.supported_features.is_empty(),
                "{} has no supported features",
                p.language
            );
            assert!(
                (0.0..=1.0).contains(&p.confidence_floor),
                "{} confidence_floor out of range: {}",
                p.language,
                p.confidence_floor
            );
        }
    }

    #[test]
    fn test_java_is_symbolic_only() {
        let p = LanguageCapabilityProfile::for_language(Language::Java);
        assert_eq!(p.capability_level, CapabilityLevel::Symbolic);
    }

    #[test]
    fn test_dataflow_languages_have_access_path() {
        for lang in &[
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            // ArkTS delegates to the TypeScript frontend but does NOT support
            // dataflow extraction, so access_path is unavailable.
        ] {
            let p = LanguageCapabilityProfile::for_language(*lang);
            assert!(
                p.supported_features.contains(&"access_path".to_string()),
                "{} should support access_path",
                p.language
            );
        }
    }

    // ── FeatureSupport tests ────────────────────────────────────────────

    #[test]
    fn test_feature_support_is_supported() {
        let s = FeatureSupport::supported();
        assert!(s.is_supported());
        assert_eq!(s.confidence_floor(), Some(0.5));

        let s = FeatureSupport::supported_with_confidence(0.7);
        assert!(s.is_supported());
        assert_eq!(s.confidence_floor(), Some(0.7));

        let s = FeatureSupport::unsupported("not implemented");
        assert!(!s.is_supported());
        assert_eq!(s.confidence_floor(), None);
    }

    #[test]
    fn test_feature_support_with_limitations() {
        let s = FeatureSupport::supported_with_limitations(0.6, vec!["limit1", "limit2"]);
        assert!(s.is_supported());
        assert_eq!(s.confidence_floor(), Some(0.6));
        if let FeatureSupport::Supported { limitations, .. } = s {
            assert_eq!(limitations.len(), 2);
        } else {
            panic!("expected Supported variant");
        }
    }

    // ── FeatureMatrix tests ─────────────────────────────────────────────

    #[test]
    fn test_feature_matrix_derive_capability_level() {
        // TS: both local_dataflow and use_def supported → DataflowBasic
        let ts = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let matrix = ts.features.as_ref().expect("TS should have FeatureMatrix");
        assert_eq!(
            matrix.derive_capability_level(),
            CapabilityLevel::DataflowBasic
        );

        // Java: no dataflow → Symbolic
        let java = LanguageCapabilityProfile::for_language(Language::Java);
        let matrix = java
            .features
            .as_ref()
            .expect("Java should have FeatureMatrix");
        assert_eq!(matrix.derive_capability_level(), CapabilityLevel::Symbolic);
    }

    #[test]
    fn test_all_profiles_have_feature_matrix() {
        let profiles = LanguageCapabilityProfile::all_compiled();
        for p in &profiles {
            assert!(
                p.features.is_some(),
                "{} should have FeatureMatrix",
                p.language
            );
        }
    }

    #[test]
    fn test_ts_feature_matrix_local_dataflow_supported() {
        let ts = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let matrix = ts.features.as_ref().unwrap();
        assert!(
            matrix.local_dataflow.is_supported(),
            "TS local_dataflow should be supported"
        );
        assert!(
            matrix.lexical_bindings.is_supported(),
            "TS lexical_bindings should be supported"
        );
        assert!(
            matrix.call_graph.is_supported(),
            "TS call_graph should be supported"
        );
        assert!(
            !matrix.interprocedural_summaries.is_supported(),
            "TS interprocedural should be unsupported"
        );
    }

    #[test]
    fn test_python_feature_matrix_lexical_bindings_unsupported() {
        let py = LanguageCapabilityProfile::for_language(Language::Python);
        let matrix = py.features.as_ref().unwrap();
        assert!(
            !matrix.lexical_bindings.is_supported(),
            "Python lexical_bindings should be unsupported"
        );
        assert!(
            matrix.local_dataflow.is_supported(),
            "Python local_dataflow should be supported"
        );
        assert!(
            !matrix.cfg.is_supported(),
            "Python cfg should be unsupported"
        );
    }

    #[test]
    fn test_java_feature_matrix_dataflow_unsupported() {
        let java = LanguageCapabilityProfile::for_language(Language::Java);
        let matrix = java.features.as_ref().unwrap();
        assert!(
            !matrix.local_dataflow.is_supported(),
            "Java local_dataflow should be unsupported"
        );
        assert!(
            matrix.call_graph.is_supported(),
            "Java call_graph should be supported"
        );
        assert!(
            !matrix.use_def.is_supported(),
            "Java use_def should be unsupported"
        );
    }

    #[test]
    fn test_cangjie_feature_matrix_no_call_graph() {
        let cj = LanguageCapabilityProfile::for_language(Language::Cangjie);
        let matrix = cj.features.as_ref().unwrap();
        assert!(
            !matrix.call_graph.is_supported(),
            "Cangjie call_graph should be unsupported"
        );
        assert!(
            matrix.symbols.is_supported(),
            "Cangjie symbols should be supported"
        );
    }

    #[test]
    fn test_feature_matrix_min_confidence_floor() {
        let ts = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let matrix = ts.features.as_ref().unwrap();
        let min = matrix.min_confidence_floor();
        assert!(
            (0.0..=1.0).contains(&min),
            "min_confidence_floor should be in [0,1], got {}",
            min
        );
    }
}
