//! Language capability profiles: declare what analysis features each language
//! supports and the confidence level at which they are delivered.
//!
//! These profiles are consumed by `atlas status`, `atlas doctor`, MCP tools
//! (`atlas_status`, `atlas_language_capabilities`), and the trace layer to
//! set user expectations about accuracy and completeness.

use serde::{Deserialize, Serialize};

use crate::types::enums::Language;

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
    pub supported_features: Vec<String>,
    /// Capabilities that the pipeline cannot yet provide for this language.
    pub unsupported_features: Vec<String>,
    /// Known accuracy / completeness caveats.
    pub limitations: Vec<String>,
    /// Floor confidence value (0.0–1.0) for edges produced in this language.
    /// Consumers should treat edges below this threshold as best-effort.
    pub confidence_floor: f64,
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
        }
    }

    // ---- ArkTS ------------------------------------------------------------

    fn arkts_profile() -> LanguageCapabilityProfile {
        // ArkTS delegates to the TypeScript adapter — same dataflow path.
        LanguageCapabilityProfile {
            language: "arkts".into(),
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
                "delegates to TypeScript adapter (ArkTS-specific constructs may be missed)".into(),
                "name-based binding (no proper shadowing)".into(),
            ],
            confidence_floor: 0.50,
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
        assert!(!profiles.is_empty(), "should have at least one compiled language");
        for p in &profiles {
            assert!(!p.supported_features.is_empty(), "{} has no supported features", p.language);
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
        for lang in &[Language::TypeScript, Language::JavaScript, Language::Python, Language::ArkTS] {
            let p = LanguageCapabilityProfile::for_language(*lang);
            assert!(
                p.supported_features.contains(&"access_path".to_string()),
                "{} should support access_path",
                p.language
            );
        }
    }
}
