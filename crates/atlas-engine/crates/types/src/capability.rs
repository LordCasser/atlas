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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    fn named_features(&self) -> [(&'static str, &FeatureSupport); 13] {
        [
            ("symbol_extraction", &self.symbols),
            ("reference_extraction", &self.references),
            ("import_resolution", &self.imports),
            ("scope_extraction", &self.scopes),
            ("call_graph", &self.call_graph),
            ("lexical_bindings", &self.lexical_bindings),
            ("intra_statement_dataflow", &self.local_dataflow),
            ("use_def_heuristic", &self.use_def),
            ("access_path", &self.field_access),
            ("call_arguments", &self.call_arguments),
            ("return_flow", &self.returns_flow),
            ("cfg", &self.cfg),
            ("interprocedural_dataflow", &self.interprocedural_summaries),
        ]
    }

    /// Returns the coarse [`CapabilityLevel`] derived from the matrix.
    ///
    /// This preserves backward compatibility with code that checks
    /// `level >= DataflowLocal`.
    pub fn derive_capability_level(&self) -> CapabilityLevel {
        let has_dataflow = self.local_dataflow.is_supported() && self.use_def.is_supported();

        if has_dataflow
            && self.interprocedural_summaries.is_supported()
            && self.returns_flow.is_supported()
            && self.call_arguments.is_supported()
        {
            CapabilityLevel::DataflowInterproc
        } else if has_dataflow {
            CapabilityLevel::DataflowLocal
        } else if self.symbols.is_supported() && self.references.is_supported() {
            CapabilityLevel::Symbolic
        } else {
            CapabilityLevel::None
        }
    }

    /// Returns the minimum confidence floor across all supported features.
    pub fn min_confidence_floor(&self) -> f64 {
        self.named_features()
            .into_iter()
            .filter_map(|(_, support)| support.confidence_floor())
            .fold(1.0, f64::min)
    }

    /// Human-readable feature names that are currently supported.
    pub fn supported_feature_names(&self) -> Vec<String> {
        self.named_features()
            .into_iter()
            .filter(|(_, support)| support.is_supported())
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// Human-readable feature names that are NOT currently supported.
    pub fn unsupported_feature_names(&self) -> Vec<String> {
        self.named_features()
            .into_iter()
            .filter(|(_, support)| !support.is_supported())
            .map(|(name, _)| name.to_string())
            .collect()
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
    /// Lexical bindings + AST-driven intra-statement dataflow (heuristic
    /// name-based binding with language-specific gaps). Use-def resolution
    /// exists but may miss shadowed variables or complex expression trees.
    DataflowLocal,
    /// Cross-statement use-def (scope-aware, shadowing-safe), backward trace
    /// with access-path chains, caller-path exploration, interprocedural flow.
    DataflowInterproc,
}

impl CapabilityLevel {
    /// Short human-readable string matching the serde variant name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Symbolic => "symbolic",
            Self::DataflowLocal => "dataflow_local",
            Self::DataflowInterproc => "dataflow_interproc",
        }
    }

    /// Parse from a lower-case string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "symbolic" => Some(Self::Symbolic),
            "dataflow_local" => Some(Self::DataflowLocal),
            "dataflow_interproc" => Some(Self::DataflowInterproc),
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
    /// "intra_statement_dataflow", "cfg"). Kept as a human-readable mirror
    /// of [`Self::features`]; capability gates must use the typed matrix.
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
    pub features: FeatureMatrix,
}

impl LanguageCapabilityProfile {
    /// Look up the static profile for every language compiled into the binary.
    pub fn for_language(lang: Language) -> Self {
        profiles::make(lang)
    }

    /// All profiles for the languages whose tree-sitter grammars are compiled in.
    #[allow(clippy::vec_init_then_push)]
    pub fn all_compiled() -> Vec<Self> {
        let mut profiles = Vec::with_capacity(14);

        #[cfg(feature = "typescript")]
        profiles.push(Self::for_language(Language::TypeScript));
        #[cfg(feature = "javascript")]
        profiles.push(Self::for_language(Language::JavaScript));
        #[cfg(feature = "python")]
        profiles.push(Self::for_language(Language::Python));
        #[cfg(feature = "java")]
        profiles.push(Self::for_language(Language::Java));
        #[cfg(feature = "c")]
        profiles.push(Self::for_language(Language::C));
        #[cfg(feature = "cpp")]
        profiles.push(Self::for_language(Language::Cpp));
        #[cfg(feature = "arkts")]
        profiles.push(Self::for_language(Language::ArkTS));
        #[cfg(feature = "cangjie")]
        profiles.push(Self::for_language(Language::Cangjie));
        #[cfg(feature = "go")]
        profiles.push(Self::for_language(Language::Go));
        #[cfg(feature = "csharp")]
        profiles.push(Self::for_language(Language::CSharp));
        #[cfg(feature = "rust")]
        profiles.push(Self::for_language(Language::Rust));
        #[cfg(feature = "php")]
        profiles.push(Self::for_language(Language::Php));
        #[cfg(feature = "ruby")]
        profiles.push(Self::for_language(Language::Ruby));
        #[cfg(feature = "kotlin")]
        profiles.push(Self::for_language(Language::Kotlin));

        profiles
    }
}

// ---------------------------------------------------------------------------
// Static profile definitions
// ---------------------------------------------------------------------------

mod profiles {
    use std::collections::HashMap;

    use super::*;

    // ── ProfileSpec data-declaration prototype ───────────────────────────

    /// Per-feature field identifier, maps 1:1 to [`FeatureMatrix`] fields.
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
    enum FeatureField {
        Symbols,
        References,
        Imports,
        Scopes,
        CallGraph,
        LexicalBindings,
        LocalDataflow,
        UseDef,
        FieldAccess,
        CallArguments,
        ReturnsFlow,
        Cfg,
        InterproceduralSummaries,
    }

    /// How a single feature's [`FeatureSupport`] differs from the default
    /// `supported_with_confidence(confidence_floor)`.
    enum FeatureOverride {
        /// Override the confidence floor for this feature (no limitations).
        /// Retained for completeness; every current spec that changes a
        /// feature's confidence also attaches limitations via
        /// [`FeatureOverride::WithLimitations`].
        #[allow(dead_code)]
        Confidence(f64),
        /// Supported with specific limitations.
        WithLimitations(f64, &'static [&'static str]),
        /// Feature is unsupported with one or more reasons (joined).
        Unsupported(&'static [&'static str]),
    }

    /// Compact per-language spec — expanded into [`LanguageCapabilityProfile`]
    /// by [`build_profile`].
    struct ProfileSpec {
        language: &'static str,
        confidence_floor: f64,
        /// Supported feature names (e.g. "symbol_extraction").
        supported: &'static [&'static str],
        /// Unsupported feature names.
        unsupported: &'static [&'static str],
        /// Known accuracy / completeness caveats.
        limitations: &'static [&'static str],
        /// Per-feature [`FeatureSupport`] overrides. Missing entries use the
        /// [`confidence_floor`] default.
        feature_overrides: &'static [(FeatureField, FeatureOverride)],
    }

    /// Build a full [`LanguageCapabilityProfile`] from a compact [`ProfileSpec`].
    fn build_profile(spec: &ProfileSpec) -> LanguageCapabilityProfile {
        let mut overrides: HashMap<FeatureField, FeatureSupport> = HashMap::new();
        for (field, ov) in spec.feature_overrides {
            let fs = match ov {
                FeatureOverride::Confidence(c) => FeatureSupport::supported_with_confidence(*c),
                FeatureOverride::WithLimitations(c, lims) => {
                    FeatureSupport::supported_with_limitations(*c, lims.to_vec())
                }
                FeatureOverride::Unsupported(reasons) => {
                    FeatureSupport::unsupported(&reasons.join("; "))
                }
            };
            overrides.insert(*field, fs);
        }

        let default = || FeatureSupport::supported_with_confidence(spec.confidence_floor);

        let fm = FeatureMatrix {
            symbols: overrides
                .get(&FeatureField::Symbols)
                .cloned()
                .unwrap_or_else(&default),
            references: overrides
                .get(&FeatureField::References)
                .cloned()
                .unwrap_or_else(&default),
            imports: overrides
                .get(&FeatureField::Imports)
                .cloned()
                .unwrap_or_else(&default),
            scopes: overrides
                .get(&FeatureField::Scopes)
                .cloned()
                .unwrap_or_else(&default),
            call_graph: overrides
                .get(&FeatureField::CallGraph)
                .cloned()
                .unwrap_or_else(&default),
            lexical_bindings: overrides
                .get(&FeatureField::LexicalBindings)
                .cloned()
                .unwrap_or_else(&default),
            local_dataflow: overrides
                .get(&FeatureField::LocalDataflow)
                .cloned()
                .unwrap_or_else(&default),
            use_def: overrides
                .get(&FeatureField::UseDef)
                .cloned()
                .unwrap_or_else(&default),
            field_access: overrides
                .get(&FeatureField::FieldAccess)
                .cloned()
                .unwrap_or_else(&default),
            call_arguments: overrides
                .get(&FeatureField::CallArguments)
                .cloned()
                .unwrap_or_else(&default),
            returns_flow: overrides
                .get(&FeatureField::ReturnsFlow)
                .cloned()
                .unwrap_or_else(&default),
            cfg: overrides
                .get(&FeatureField::Cfg)
                .cloned()
                .unwrap_or_else(&default),
            interprocedural_summaries: overrides
                .get(&FeatureField::InterproceduralSummaries)
                .cloned()
                .unwrap_or_else(&default),
        };

        LanguageCapabilityProfile {
            language: spec.language.into(),
            capability_level: fm.derive_capability_level(),
            supported_features: spec.supported.iter().map(|s| s.to_string()).collect(),
            unsupported_features: spec.unsupported.iter().map(|s| s.to_string()).collect(),
            limitations: spec.limitations.iter().map(|s| s.to_string()).collect(),
            confidence_floor: spec.confidence_floor,
            features: fm,
        }
    }

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
            Language::Kotlin => kotlin_profile(),
        }
    }

    // ---- TypeScript -------------------------------------------------------

    const TS_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "typescript",
        confidence_floor: 0.60,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
            "AST-driven local dataflow with language-specific gaps",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
                    ],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.60,
                    &["AST-driven local dataflow; destructuring and async not yet path-verified"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.60,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                        "indirect callers limited to depth 3 (runtime fallback)",
                    ],
                ),
            ),
        ],
    };

    fn ts_profile() -> LanguageCapabilityProfile {
        build_profile(&TS_PROFILE_SPEC)
    }

    // ---- JavaScript -------------------------------------------------------

    // JavaScript shares the TypeScript adapter; same capabilities/limits.
    const JS_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "javascript",
        confidence_floor: 0.60,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "shares TypeScript adapter (TSX-only constructs may trigger warnings)",
            "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
            "AST-driven local dataflow with language-specific gaps",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
                    ],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.60,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.60,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "cross-function bridges via summary tables (ReturnToCall verified; ArgToParam not yet verified)",
                        "indirect callers limited to depth 3 (runtime fallback)",
                    ],
                ),
            ),
        ],
    };

    fn js_profile() -> LanguageCapabilityProfile {
        build_profile(&JS_PROFILE_SPEC)
    }

    // ---- Python (DataflowInterproc) ---------------------------------------------
    // NOTE: Golden fixtures fx21 (ArgToParam), fx22 (ReturnToCall),
    //       fx_py_shadow (shadowing precision), and fx_py_destructure
    //       (tuple unpacking) exist.
    //       Confidence raised from 0.65 to 0.72: scope-chain-aware binding
    //       resolution (resolve_bindings_to_nodes) correctly handles Python
    //       shadowing as verified by fx_py_shadow.  CFG support added (P7).

    const PY_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "python",
        confidence_floor: 0.72,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware use-def with binding_id grouping; edge cases in nested dynamic scopes",
            "AST-driven local dataflow; destructuring and control-flow not yet path-verified",
            "assignment LHS binding with scope-chain resolution",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.72,
                    &["assignment LHS binding with scope-chain resolution"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "AST-driven local dataflow; destructuring and control-flow not yet path-verified",
                    ],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "scope-chain-aware use-def with binding_id grouping; edge cases in nested dynamic scopes",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn py_profile() -> LanguageCapabilityProfile {
        build_profile(&PY_PROFILE_SPEC)
    }

    // ---- Java (DataflowInterproc) -------------------------------------------------

    const JAVA_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "java",
        confidence_floor: 0.75,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
            "AST-driven local dataflow with language-specific gaps",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.75,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                    ],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.75,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.75,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.75,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.75,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn java_profile() -> LanguageCapabilityProfile {
        build_profile(&JAVA_PROFILE_SPEC)
    }

    // ---- C (DataflowInterproc) --------------------------------------------------
    // NOTE: Golden fixtures fx23 (ArgToParam) and fx24 (ReturnToCall) exist.
    //       Bridge behavior may vary — gaps documented via should_panic if
    //       fixtures fail.
    //       Confidence raised from 0.67 to 0.73: CFG support added (P7),
    //       binding description updated to scope-chain-aware.
    //
    // Special cases preserved in the supported string list (independent of the
    // FeatureMatrix): "include_resolution" (not "import_resolution") and
    // "function_pointer_tracking" (no matching FeatureMatrix field). The
    // matrix `imports` field still uses the default confidence.

    const C_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "c",
        confidence_floor: 0.73,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "include_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "function_pointer_tracking",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support",
            "AST-driven local dataflow with language-specific gaps",
            "macro expansion and #include resolution may produce incomplete facts",
            "function pointer calls resolved via local def-use chain (depth 3); inter-procedural pointer flow not tracked",
        ],
        feature_overrides: &[
            (
                FeatureField::CallGraph,
                FeatureOverride::WithLimitations(
                    0.65,
                    &[
                        "function pointer calls resolved via local def-use (depth 3, intra-procedural only)",
                    ],
                ),
            ),
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.73,
                    &["scope-chain-aware binding with shadowing support"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.73,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.73,
                    &["scope-chain-aware binding with shadowing support"],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.73,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.73,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn c_profile() -> LanguageCapabilityProfile {
        build_profile(&C_PROFILE_SPEC)
    }

    // ---- C++ (DataflowInterproc) ------------------------------------------------
    // NOTE: Golden fixtures fx25 (ArgToParam) and fx26 (ReturnToCall) exist.
    //       Bridge behavior may vary — gaps documented via should_panic if
    //       fixtures fail.
    //       Confidence raised from 0.62 to 0.70: CFG support added (P7),
    //       binding description updated to scope-chain-aware.
    //
    // Special case preserved in the supported string list (independent of the
    // FeatureMatrix): "include_resolution" (not "import_resolution").

    const CPP_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "cpp",
        confidence_floor: 0.70,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "include_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support",
            "AST-driven local dataflow with language-specific gaps",
            "template instantiation not followed",
            "ADL and overload resolution not modeled",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["scope-chain-aware binding with shadowing support"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["scope-chain-aware binding with shadowing support"],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.70,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn cpp_profile() -> LanguageCapabilityProfile {
        build_profile(&CPP_PROFILE_SPEC)
    }

    // ---- ArkTS (DataflowInterproc) ---------------------------------------------
    // NOTE: Golden fixtures fx27 (ArgToParam), fx28 (ReturnToCall),
    //       fx30 (@Component decorator), and fx31 (class-as-struct) exist.
    //       ArkTS delegates to the TypeScript frontend with byte-stable `struct`
    //       normalization. Declarative members and UI call ownership are verified.
    //       ArkUI trailing-block syntax still yields local parse errors. Query-time
    //       tracing bridges verified AppStorage writes into reactive field reads.
    //
    // ArkTS delegates core syntax to the TypeScript frontend. Atlas does not run
    // the ArkTS compiler, so migration-guide restrictions cannot be treated as
    // validated input invariants. ArkUI trailing blocks and nested callbacks
    // remain explicit analysis boundaries.

    const ARKTS_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "arkts",
        confidence_floor: 0.60,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "interprocedural_dataflow",
            "cfg",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "TS grammar fallback with byte-stable struct/member recovery; ArkUI trailing-block syntax may retain partial parse status",
            "scope-chain binding is heuristic and does not independently model ArkUI callback ownership",
            "AppStorage set/setOrCreate to StorageProp/StorageLink uses exact syntactic key matching; reverse writes, default initialization, and process boundaries are not modeled",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "scope-chain-aware binding via TS grammar; ArkUI callback ownership is not independently symbolized",
                    ],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "dataflow via TS grammar; ArkUI trailing-block and nested callback internals remain best-effort",
                    ],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "scope-chain-aware use-def via TS grammar; no ArkTS compiler validation is performed",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.55,
                    &[
                        "named function and method branch/loop body traversal implemented via TS grammar; ArkUI trailing blocks collapse to Statement nodes and nested arrow callbacks do not get independent CFGs",
                        "switch/case and try/catch CFG subgraphs are deferred (shared TS limitation)",
                    ],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.60,
                    &[
                        "ArgToParam and ReturnToCall are verified for resolved calls; framework callbacks and polymorphic targets remain best-effort",
                    ],
                ),
            ),
        ],
    };

    fn arkts_profile() -> LanguageCapabilityProfile {
        build_profile(&ARKTS_PROFILE_SPEC)
    }

    // ---- Cangjie ----------------------------------------------------------
    // Confidence raised from 0.62 to 0.65: method calls (obj.method())
    // now captured via postfixExpression(fieldAccess, callSuffix) pattern.
    //
    // Migration note: the previous implementation derived supported/unsupported
    // from `fm.supported_feature_names()` / `fm.unsupported_feature_names()`.
    // Because every feature is supported for Cangjie, that derivation produced
    // all 13 `FeatureMatrix::named_features()` names (including
    // "scope_extraction", which the other languages' hand-written lists omit)
    // and an empty unsupported list. Those exact lists are encoded explicitly
    // below to preserve behavior.

    const CANGJIE_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "cangjie",
        confidence_floor: 0.65,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "scope_extraction",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &[],
        limitations: &[
            "AST-driven local dataflow with basic parameter/local/return/call capture",
            "method call targets now captured (simple + obj.method() patterns)",
            "scope-chain-aware binding not implemented",
        ],
        feature_overrides: &[
            (
                FeatureField::CallGraph,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["simple function calls + method calls (obj.method())"],
                ),
            ),
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["basic parameter/local binding extraction"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(0.65, &["AST-driven local dataflow"]),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["basic use-def via lexical bindings + dataflow"],
                ),
            ),
            (
                FeatureField::FieldAccess,
                FeatureOverride::WithLimitations(0.55, &["basic field access capture"]),
            ),
            (
                FeatureField::CallArguments,
                FeatureOverride::WithLimitations(0.65, &["basic call argument capture"]),
            ),
            (
                FeatureField::ReturnsFlow,
                FeatureOverride::WithLimitations(0.65, &["basic return value capture"]),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.60,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.65,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall basic)",
                    ],
                ),
            ),
        ],
    };

    fn cangjie_profile() -> LanguageCapabilityProfile {
        build_profile(&CANGJIE_PROFILE_SPEC)
    }

    // ---- Go (DataflowInterproc) --------------------------------------------------

    const GO_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "go",
        confidence_floor: 0.78,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
            "AST-driven local dataflow with language-specific gaps",
            "generic type parameters not captured in dataflow layer",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.78,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                    ],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.78,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.78,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.78,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.78,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn go_profile() -> LanguageCapabilityProfile {
        build_profile(&GO_PROFILE_SPEC)
    }

    // ---- C# (DataflowInterproc) ---------------------------------------------------

    const CSHARP_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "csharp",
        confidence_floor: 0.72,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
            "AST-driven local dataflow with language-specific gaps",
            "partial classes across files not merged",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                    ],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.72,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                    ],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "Control-flow graph with using_statement and branch/loop body traversal implemented",
                    ],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.72,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn csharp_profile() -> LanguageCapabilityProfile {
        build_profile(&CSHARP_PROFILE_SPEC)
    }

    // ---- Rust (DataflowInterproc) -----------------------------------------------
    // NOTE: Both ArgToParam (fx13) and ReturnToCall (fx14) cross-function
    //       bridges verified against golden fixtures.  Upgraded to
    //       DataflowInterproc.  Confidence raised from 0.62 to 0.70: CFG support
    //       added (P7), binding description updated to scope-chain-aware.

    const RUST_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "rust",
        confidence_floor: 0.70,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "scope-chain-aware binding with shadowing support",
            "AST-driven local dataflow with language-specific gaps",
            "macro_rules! body patterns not analyzed",
            "borrow checker semantics not modeled",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["scope-chain-aware binding with shadowing support"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["scope-chain-aware binding with shadowing support"],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.70,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.70,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn rust_profile() -> LanguageCapabilityProfile {
        build_profile(&RUST_PROFILE_SPEC)
    }

    // ---- PHP (DataflowInterproc) --------------------------------------------------
    // NOTE: PHP ArgToParam (fx15) and ReturnToCall (fx16) bridges verified.

    const PHP_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "php",
        confidence_floor: 0.62,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "interprocedural_dataflow",
        ],
        unsupported: &["cfg", "scope_aware_binding"],
        limitations: &[
            "name-based binding (no proper shadowing)",
            "AST-driven local dataflow with language-specific gaps",
            "dynamic method calls via variable emit low-confidence edges (not yet resolved)",
            "namespace aliases resolved at reference resolution layer",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.62,
                    &["name-based binding (no proper shadowing)"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.62,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.62,
                    &["name-based binding (no proper shadowing)"],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::Unsupported(&["CFG builder not implemented for PHP"]),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.62,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn php_profile() -> LanguageCapabilityProfile {
        build_profile(&PHP_PROFILE_SPEC)
    }

    // ---- Ruby (DataflowInterproc) -------------------------------------------------
    // NOTE: ArgToParam bridge fires (fx17 passes); ReturnToCall bridge (fx18) and
    //       basic local dataflow (fx32) also verified.  Upgraded to DataflowInterproc;
    //       gaps tracked via golden fixtures.  CFG support added for
    //       block-managed resource lifecycle (File.open { |f| ... }).
    //
    // NOTE: "cfg" intentionally appears last in the supported list (after
    // "interprocedural_dataflow"), matching the original hand-written order.

    const RUBY_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "ruby",
        confidence_floor: 0.65,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "interprocedural_dataflow",
            "cfg",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "name-based binding (no proper shadowing)",
            "AST-driven local dataflow with language-specific gaps",
            "dynamic methods (method_missing / define_method) not yet verified",
            "block/yield implicit calls documented but not yet implemented",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["name-based binding (no proper shadowing)"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["name-based binding (no proper shadowing)"],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.65,
                    &["CFG with block body traversal implemented for Ruby block-managed resources"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.65,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn ruby_profile() -> LanguageCapabilityProfile {
        build_profile(&RUBY_PROFILE_SPEC)
    }

    // ---- Kotlin (DataflowInterproc) -----------------------------------------------
    // NOTE: Golden fixtures fx19 (ArgToParam) and fx20 (ReturnToCall) exist.
    //       Bridge behavior may vary — gaps documented via should_panic if
    //       fixtures fail.
    //       Extension function receiver (fun String.isValid()) now creates a
    //       "this" binding as the first parameter.

    const KOTLIN_PROFILE_SPEC: ProfileSpec = ProfileSpec {
        language: "kotlin",
        confidence_floor: 0.67,
        supported: &[
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ],
        unsupported: &["scope_aware_binding"],
        limitations: &[
            "name-based binding (no proper shadowing)",
            "AST-driven local dataflow with language-specific gaps",
            "extension functions: receiver creates 'this' binding; type-directed resolution not yet done",
        ],
        feature_overrides: &[
            (
                FeatureField::LexicalBindings,
                FeatureOverride::WithLimitations(
                    0.67,
                    &["name-based binding (no proper shadowing)"],
                ),
            ),
            (
                FeatureField::LocalDataflow,
                FeatureOverride::WithLimitations(
                    0.67,
                    &["AST-driven local dataflow with language-specific gaps"],
                ),
            ),
            (
                FeatureField::UseDef,
                FeatureOverride::WithLimitations(
                    0.67,
                    &["name-based binding (no proper shadowing)"],
                ),
            ),
            (
                FeatureField::Cfg,
                FeatureOverride::WithLimitations(
                    0.67,
                    &["Control-flow graph with branch/loop body traversal implemented"],
                ),
            ),
            (
                FeatureField::InterproceduralSummaries,
                FeatureOverride::WithLimitations(
                    0.67,
                    &[
                        "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    ],
                ),
            ),
        ],
    };

    fn kotlin_profile() -> LanguageCapabilityProfile {
        build_profile(&KOTLIN_PROFILE_SPEC)
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
            CapabilityLevel::DataflowLocal,
            CapabilityLevel::DataflowInterproc,
        ] {
            let s = level.as_str();
            let parsed = CapabilityLevel::from_str(s);
            assert_eq!(parsed, Some(*level), "roundtrip failed for {level:?}");
        }
    }

    #[test]
    fn test_ts_profile_exists() {
        let p = LanguageCapabilityProfile::for_language(Language::TypeScript);
        assert_eq!(p.language, "typescript");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
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
    fn test_java_is_dataflow_interproc() {
        let p = LanguageCapabilityProfile::for_language(Language::Java);
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
    }

    #[test]
    fn test_dataflow_languages_have_access_path() {
        for lang in &[
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            // ArkTS is DataflowInterproc (delegates to TypeScript frontend, confidence 0.50).
        ] {
            let p = LanguageCapabilityProfile::for_language(*lang);
            assert!(
                p.features.field_access.is_supported(),
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
        // TS: DataflowInterproc (all features including interprocedural_summaries supported)
        let ts = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let matrix = &ts.features;
        assert_eq!(
            matrix.derive_capability_level(),
            CapabilityLevel::DataflowInterproc
        );

        // Java: all DataflowInterproc preconditions met (local_dataflow + use_def + interprocedural_summaries + returns_flow + call_arguments)
        let java = LanguageCapabilityProfile::for_language(Language::Java);
        let matrix = &java.features;
        assert_eq!(
            matrix.derive_capability_level(),
            CapabilityLevel::DataflowInterproc
        );
    }

    #[test]
    fn test_feature_matrix_derive_dataflow_interproc() {
        // Construct a synthetic FeatureMatrix where all DataflowInterproc
        // preconditions are met.
        let matrix = FeatureMatrix {
            symbols: FeatureSupport::supported_with_confidence(0.8),
            references: FeatureSupport::supported_with_confidence(0.8),
            imports: FeatureSupport::supported_with_confidence(0.8),
            scopes: FeatureSupport::supported_with_confidence(0.8),
            call_graph: FeatureSupport::supported_with_confidence(0.8),
            lexical_bindings: FeatureSupport::supported_with_confidence(0.8),
            local_dataflow: FeatureSupport::supported_with_confidence(0.8),
            use_def: FeatureSupport::supported_with_confidence(0.8),
            field_access: FeatureSupport::supported_with_confidence(0.8),
            call_arguments: FeatureSupport::supported_with_confidence(0.8),
            returns_flow: FeatureSupport::supported_with_confidence(0.8),
            cfg: FeatureSupport::unsupported("not implemented"),
            interprocedural_summaries: FeatureSupport::supported_with_confidence(0.8),
        };
        assert_eq!(
            matrix.derive_capability_level(),
            CapabilityLevel::DataflowInterproc,
            "full matrix should derive DataflowInterproc"
        );
    }

    #[test]
    fn test_feature_matrix_derive_dataflow_local_when_summaries_missing() {
        // All dataflow features present EXCEPT interprocedural summaries.
        let matrix = FeatureMatrix {
            symbols: FeatureSupport::supported_with_confidence(0.8),
            references: FeatureSupport::supported_with_confidence(0.8),
            imports: FeatureSupport::supported_with_confidence(0.8),
            scopes: FeatureSupport::supported_with_confidence(0.8),
            call_graph: FeatureSupport::supported_with_confidence(0.8),
            lexical_bindings: FeatureSupport::supported_with_confidence(0.8),
            local_dataflow: FeatureSupport::supported_with_confidence(0.8),
            use_def: FeatureSupport::supported_with_confidence(0.8),
            field_access: FeatureSupport::supported_with_confidence(0.8),
            call_arguments: FeatureSupport::supported_with_confidence(0.8),
            returns_flow: FeatureSupport::supported_with_confidence(0.8),
            cfg: FeatureSupport::unsupported("not implemented"),
            interprocedural_summaries: FeatureSupport::unsupported("not implemented"),
        };
        assert_eq!(
            matrix.derive_capability_level(),
            CapabilityLevel::DataflowLocal,
            "without summaries should still be DataflowLocal"
        );
    }

    #[test]
    fn test_all_profiles_have_feature_matrix() {
        let profiles = LanguageCapabilityProfile::all_compiled();
        for p in &profiles {
            assert!(
                p.features.symbols.is_supported(),
                "{} should expose a typed FeatureMatrix",
                p.language
            );
        }
    }

    #[test]
    fn test_ts_feature_matrix_local_dataflow_supported() {
        let ts = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let matrix = &ts.features;
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
            matrix.interprocedural_summaries.is_supported(),
            "TS interprocedural should be supported"
        );
    }

    #[test]
    fn test_python_feature_matrix_lexical_bindings_supported() {
        let py = LanguageCapabilityProfile::for_language(Language::Python);
        let matrix = &py.features;
        assert!(
            matrix.lexical_bindings.is_supported(),
            "Python lexical_bindings should be supported"
        );
        assert!(
            matrix.local_dataflow.is_supported(),
            "Python local_dataflow should be supported"
        );
        assert!(matrix.cfg.is_supported(), "Python cfg should be supported");
    }

    #[test]
    fn test_java_feature_matrix_dataflow_supported() {
        let java = LanguageCapabilityProfile::for_language(Language::Java);
        let matrix = &java.features;
        assert!(
            matrix.local_dataflow.is_supported(),
            "Java local_dataflow should be supported"
        );
        assert!(
            matrix.call_graph.is_supported(),
            "Java call_graph should be supported"
        );
        assert!(
            matrix.use_def.is_supported(),
            "Java use_def should be supported"
        );
    }

    #[test]
    fn test_cangjie_feature_matrix_dataflow_interproc() {
        let cj = LanguageCapabilityProfile::for_language(Language::Cangjie);
        let matrix = &cj.features;
        assert_eq!(
            cj.capability_level,
            CapabilityLevel::DataflowInterproc,
            "Cangjie should be DataflowInterproc"
        );
        assert!(
            matrix.symbols.is_supported(),
            "Cangjie symbols should be supported"
        );
        assert!(
            matrix.lexical_bindings.is_supported(),
            "Cangjie lexical_bindings should be supported"
        );
        assert!(
            matrix.local_dataflow.is_supported(),
            "Cangjie local_dataflow should be supported"
        );
        assert!(
            matrix.call_graph.is_supported(),
            "Cangjie call_graph should be supported (callsite extraction works)"
        );
        assert!(
            matrix.interprocedural_summaries.is_supported(),
            "Cangjie interprocedural_summaries should be supported"
        );
    }

    #[test]
    fn test_feature_matrix_min_confidence_floor() {
        let ts = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let matrix = &ts.features;
        let min = matrix.min_confidence_floor();
        assert!(
            (0.0..=1.0).contains(&min),
            "min_confidence_floor should be in [0,1], got {min}"
        );
    }

    #[test]
    fn test_cfg_known_limitation() {
        for profile in LanguageCapabilityProfile::all_compiled() {
            let fm = &profile.features;
            if fm.cfg.is_supported() {
                // Every supported CFG should declare body traversal as implemented
                let msg = format!(
                    "Language {}: CFG is supported but body traversal not declared as implemented",
                    profile.language
                );
                assert!(has_cfg_body_traversal_implemented(&fm.cfg), "{}", msg);
            }
        }
    }

    fn has_cfg_body_traversal_implemented(fs: &FeatureSupport) -> bool {
        match fs {
            FeatureSupport::Supported { limitations, .. } => limitations
                .iter()
                .any(|l| l.contains("body traversal") && l.contains("implemented")),
            FeatureSupport::Unsupported { .. } => true,
        }
    }

    /// Regression guard: runtime gates use `FeatureMatrix.cfg.is_supported()`,
    /// and the human-readable `supported_features` mirror must stay aligned.
    ///
    /// This test iterates all compiled language profiles and asserts that
    /// `FeatureMatrix.cfg.is_supported()` and the presence of `"cfg"` in
    /// `supported_features` are always consistent.
    #[test]
    fn test_cfg_feature_matrix_consistent_with_supported_features() {
        let profiles = LanguageCapabilityProfile::all_compiled();
        assert!(
            !profiles.is_empty(),
            "need at least one compiled language profile"
        );

        for profile in &profiles {
            let matrix = &profile.features;
            let matrix_says_cfg = matrix.cfg.is_supported();
            let string_list_says_cfg = profile.supported_features.contains(&"cfg".to_string());

            assert_eq!(
                matrix_says_cfg, string_list_says_cfg,
                "{}: FeatureMatrix.cfg.is_supported()={} but supported_features contains 'cfg'={}. \
                 The typed matrix and human-readable mirror must agree.",
                profile.language, matrix_says_cfg, string_list_says_cfg,
            );
        }
    }

    // ── ProfileSpec identity tests ───────────────────────────────────────

    /// Verify the Go profile produced by `go_profile()` matches the expected
    /// values — ensuring the ProfileSpec data-declaration is correct.
    #[test]
    fn test_go_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Go);
        assert_eq!(p.language, "go");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.78);

        // supported_features
        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);

        // unsupported_features
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);

        // limitations
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                "AST-driven local dataflow with language-specific gaps",
                "generic type parameters not captured in dataflow layer",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.78, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.78));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.78)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.78));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.78));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.78)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.78)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.78)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.78)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.78,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested expressions"
                ],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.78,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.78,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested expressions"
                ],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.78,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.78,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                ],
            )
        );
    }

    /// Verify the Python profile produced by `py_profile()` matches the expected
    /// values — including the CFG confidence override (0.70 vs 0.72 default).
    #[test]
    fn test_python_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Python);
        assert_eq!(p.language, "python");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.72);

        // supported_features
        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);

        // unsupported_features
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);

        // limitations
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware use-def with binding_id grouping; edge cases in nested dynamic scopes",
                "AST-driven local dataflow; destructuring and control-flow not yet path-verified",
                "assignment LHS binding with scope-chain resolution",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.72, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.72));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.72));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.72));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.72)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec!["assignment LHS binding with scope-chain resolution"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "AST-driven local dataflow; destructuring and control-flow not yet path-verified"
                ],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "scope-chain-aware use-def with binding_id grouping; edge cases in nested dynamic scopes"
                ],
            )
        );
        // CFG has confidence override (0.70, not 0.72)
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                ],
            )
        );
    }

    /// Verify the TypeScript profile produced by `ts_profile()` matches the
    /// expected values — including the interprocedural override (0.72 vs 0.60).
    #[test]
    fn test_typescript_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::TypeScript);
        assert_eq!(p.language, "typescript");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.60);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
                "AST-driven local dataflow with language-specific gaps",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.60, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.60)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns"
                ],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec!["AST-driven local dataflow; destructuring and async not yet path-verified"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns"
                ],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)",
                    "indirect callers limited to depth 3 (runtime fallback)",
                ],
            )
        );
    }

    /// Verify the JavaScript profile produced by `js_profile()` matches the
    /// expected values (shares the TypeScript adapter).
    #[test]
    fn test_javascript_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::JavaScript);
        assert_eq!(p.language, "javascript");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.60);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "shares TypeScript adapter (TSX-only constructs may trigger warnings)",
                "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns",
                "AST-driven local dataflow with language-specific gaps",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.60, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.60)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns"
                ],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested destructuring and async patterns"
                ],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "cross-function bridges via summary tables (ReturnToCall verified; ArgToParam not yet verified)",
                    "indirect callers limited to depth 3 (runtime fallback)",
                ],
            )
        );
    }

    /// Verify the Java profile produced by `java_profile()` matches the expected values.
    #[test]
    fn test_java_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Java);
        assert_eq!(p.language, "java");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.75);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                "AST-driven local dataflow with language-specific gaps",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.75, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.75));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.75)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.75));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.75));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.75)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.75)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.75)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.75)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.75,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested expressions"
                ],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.75,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.75,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested expressions"
                ],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.75,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.75,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the C profile produced by `c_profile()` matches the expected
    /// values — including the "include_resolution" / "function_pointer_tracking"
    /// supported-list entries and the call_graph confidence override (0.65).
    #[test]
    fn test_c_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::C);
        assert_eq!(p.language, "c");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.73);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "include_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "function_pointer_tracking",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support",
                "AST-driven local dataflow with language-specific gaps",
                "macro expansion and #include resolution may produce incomplete facts",
                "function pointer calls resolved via local def-use chain (depth 3); inter-procedural pointer flow not tracked",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.73, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.73));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.73)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.73));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.73));
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.73)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.73)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.73)
        );

        // call_graph has confidence override (0.65, not 0.73)
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec![
                    "function pointer calls resolved via local def-use (depth 3, intra-procedural only)"
                ],
            )
        );
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.73,
                vec!["scope-chain-aware binding with shadowing support"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.73,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.73,
                vec!["scope-chain-aware binding with shadowing support"],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.73,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.73,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the C++ profile produced by `cpp_profile()` matches the expected
    /// values — including the "include_resolution" supported-list entry.
    #[test]
    fn test_cpp_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Cpp);
        assert_eq!(p.language, "cpp");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.70);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "include_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support",
                "AST-driven local dataflow with language-specific gaps",
                "template instantiation not followed",
                "ADL and overload resolution not modeled",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.70, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.70));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.70));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.70));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.70)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["scope-chain-aware binding with shadowing support"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["scope-chain-aware binding with shadowing support"],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the ArkTS profile produced by `arkts_profile()` matches the
    /// expected values - including CFG as WithLimitations(0.55) and explicit
    /// ArkUI/parser boundaries for lexical/dataflow/use-def/interproc.
    #[test]
    fn test_arkts_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::ArkTS);
        assert_eq!(p.language, "arkts");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.60);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "interprocedural_dataflow",
            "cfg",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "TS grammar fallback with byte-stable struct/member recovery; ArkUI trailing-block syntax may retain partial parse status",
                "scope-chain binding is heuristic and does not independently model ArkUI callback ownership",
                "AppStorage set/setOrCreate to StorageProp/StorageLink uses exact syntactic key matching; reverse writes, default initialization, and process boundaries are not modeled",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.60, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.60));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.60)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.60)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "scope-chain-aware binding via TS grammar; ArkUI callback ownership is not independently symbolized"
                ],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "dataflow via TS grammar; ArkUI trailing-block and nested callback internals remain best-effort"
                ],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "scope-chain-aware use-def via TS grammar; no ArkTS compiler validation is performed"
                ],
            )
        );
        // CFG supported with limitations (TS grammar fallback)
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.55,
                vec![
                    "named function and method branch/loop body traversal implemented via TS grammar; ArkUI trailing blocks collapse to Statement nodes and nested arrow callbacks do not get independent CFGs",
                    "switch/case and try/catch CFG subgraphs are deferred (shared TS limitation)",
                ],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec![
                    "ArgToParam and ReturnToCall are verified for resolved calls; framework callbacks and polymorphic targets remain best-effort"
                ],
            )
        );
    }

    /// Verify the Cangjie profile produced by `cangjie_profile()` matches the
    /// expected values — including the explicit supported list (previously
    /// derived from the matrix, so it includes "scope_extraction") and the
    /// field_access (0.55) / cfg (0.60) confidence overrides.
    #[test]
    fn test_cangjie_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Cangjie);
        assert_eq!(p.language, "cangjie");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.65);

        // supported_features: previously derived from fm.supported_feature_names(),
        // which — because all features are supported — includes "scope_extraction".
        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "scope_extraction",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        // Previously derived from fm.unsupported_feature_names() — empty because
        // every feature is supported.
        assert!(p.unsupported_features.is_empty());
        assert_eq!(
            p.limitations,
            vec![
                "AST-driven local dataflow with basic parameter/local/return/call capture",
                "method call targets now captured (simple + obj.method() patterns)",
                "scope-chain-aware binding not implemented",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.65, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.65));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.65)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.65));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.65));

        // Overridden features
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["simple function calls + method calls (obj.method())"],
            )
        );
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["basic parameter/local binding extraction"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(0.65, vec!["AST-driven local dataflow"])
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["basic use-def via lexical bindings + dataflow"],
            )
        );
        // field_access has confidence override (0.55, not 0.65)
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_limitations(0.55, vec!["basic field access capture"])
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_limitations(0.65, vec!["basic call argument capture"])
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_limitations(0.65, vec!["basic return value capture"])
        );
        // cfg has confidence override (0.60, not 0.65)
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.60,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall basic)"
                ],
            )
        );
    }

    /// Verify the C# profile produced by `csharp_profile()` matches the expected
    /// values — including the CFG limitation mentioning "using_statement".
    #[test]
    fn test_csharp_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::CSharp);
        assert_eq!(p.language, "csharp");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.72);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support; edge cases in nested expressions",
                "AST-driven local dataflow with language-specific gaps",
                "partial classes across files not merged",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.72, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.72));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.72));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.72));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.72)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.72)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested expressions"
                ],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "scope-chain-aware binding with shadowing support; edge cases in nested expressions"
                ],
            )
        );
        // CFG limitation must mention both "body traversal" and "implemented"
        // (and the using_statement specialization).
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "Control-flow graph with using_statement and branch/loop body traversal implemented"
                ],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.72,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the Rust profile produced by `rust_profile()` matches the expected values.
    #[test]
    fn test_rust_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Rust);
        assert_eq!(p.language, "rust");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.70);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "scope-chain-aware binding with shadowing support",
                "AST-driven local dataflow with language-specific gaps",
                "macro_rules! body patterns not analyzed",
                "borrow checker semantics not modeled",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.70, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.70));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.70));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.70));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.70)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.70)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["scope-chain-aware binding with shadowing support"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["scope-chain-aware binding with shadowing support"],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.70,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the PHP profile produced by `php_profile()` matches the expected
    /// values — including the Unsupported CFG (in the unsupported list).
    #[test]
    fn test_php_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Php);
        assert_eq!(p.language, "php");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.62);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["cfg", "scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "name-based binding (no proper shadowing)",
                "AST-driven local dataflow with language-specific gaps",
                "dynamic method calls via variable emit low-confidence edges (not yet resolved)",
                "namespace aliases resolved at reference resolution layer",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.62, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.62));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.62)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.62));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.62));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.62)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.62)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.62)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.62)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.62,
                vec!["name-based binding (no proper shadowing)"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.62,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.62,
                vec!["name-based binding (no proper shadowing)"],
            )
        );
        // CFG unsupported
        assert_eq!(
            fm.cfg,
            FeatureSupport::unsupported("CFG builder not implemented for PHP")
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.62,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the Ruby profile produced by `ruby_profile()` matches the expected
    /// values — including the "cfg" entry ordered last in supported_features and
    /// the block-body CFG limitation ("body traversal" + "implemented").
    #[test]
    fn test_ruby_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Ruby);
        assert_eq!(p.language, "ruby");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.65);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "interprocedural_dataflow",
            "cfg",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "name-based binding (no proper shadowing)",
                "AST-driven local dataflow with language-specific gaps",
                "dynamic methods (method_missing / define_method) not yet verified",
                "block/yield implicit calls documented but not yet implemented",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.65, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.65));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.65)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.65));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.65));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.65)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.65)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.65)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.65)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["name-based binding (no proper shadowing)"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["name-based binding (no proper shadowing)"],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec!["CFG with block body traversal implemented for Ruby block-managed resources"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.65,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }

    /// Verify the Kotlin profile produced by `kotlin_profile()` matches the expected values.
    #[test]
    fn test_kotlin_profile_identity() {
        let p = LanguageCapabilityProfile::for_language(Language::Kotlin);
        assert_eq!(p.language, "kotlin");
        assert_eq!(p.capability_level, CapabilityLevel::DataflowInterproc);
        assert_eq!(p.confidence_floor, 0.67);

        let expected_supported: Vec<&str> = vec![
            "symbol_extraction",
            "reference_extraction",
            "import_resolution",
            "call_graph",
            "lexical_bindings",
            "intra_statement_dataflow",
            "use_def_heuristic",
            "access_path",
            "call_arguments",
            "return_flow",
            "cfg",
            "interprocedural_dataflow",
        ];
        assert_eq!(p.supported_features, expected_supported);
        assert_eq!(p.unsupported_features, vec!["scope_aware_binding"]);
        assert_eq!(
            p.limitations,
            vec![
                "name-based binding (no proper shadowing)",
                "AST-driven local dataflow with language-specific gaps",
                "extension functions: receiver creates 'this' binding; type-directed resolution not yet done",
            ]
        );

        let fm = &p.features;

        // Default features (confidence_floor = 0.67, no limitations)
        assert_eq!(fm.symbols, FeatureSupport::supported_with_confidence(0.67));
        assert_eq!(
            fm.references,
            FeatureSupport::supported_with_confidence(0.67)
        );
        assert_eq!(fm.imports, FeatureSupport::supported_with_confidence(0.67));
        assert_eq!(fm.scopes, FeatureSupport::supported_with_confidence(0.67));
        assert_eq!(
            fm.call_graph,
            FeatureSupport::supported_with_confidence(0.67)
        );
        assert_eq!(
            fm.field_access,
            FeatureSupport::supported_with_confidence(0.67)
        );
        assert_eq!(
            fm.call_arguments,
            FeatureSupport::supported_with_confidence(0.67)
        );
        assert_eq!(
            fm.returns_flow,
            FeatureSupport::supported_with_confidence(0.67)
        );

        // Overridden features
        assert_eq!(
            fm.lexical_bindings,
            FeatureSupport::supported_with_limitations(
                0.67,
                vec!["name-based binding (no proper shadowing)"],
            )
        );
        assert_eq!(
            fm.local_dataflow,
            FeatureSupport::supported_with_limitations(
                0.67,
                vec!["AST-driven local dataflow with language-specific gaps"],
            )
        );
        assert_eq!(
            fm.use_def,
            FeatureSupport::supported_with_limitations(
                0.67,
                vec!["name-based binding (no proper shadowing)"],
            )
        );
        assert_eq!(
            fm.cfg,
            FeatureSupport::supported_with_limitations(
                0.67,
                vec!["Control-flow graph with branch/loop body traversal implemented"],
            )
        );
        assert_eq!(
            fm.interprocedural_summaries,
            FeatureSupport::supported_with_limitations(
                0.67,
                vec![
                    "cross-function bridges via summary tables (ArgToParam verified, ReturnToCall verified)"
                ],
            )
        );
    }
}
