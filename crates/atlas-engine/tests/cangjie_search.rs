//! Integration test: cangjie feature propagation to search subsystem.
//!
//! Verifies that building `atlas-engine` with `--features cangjie` makes
//! `lang:cj` available in the search query parser, per architecture
//! constraint §9: language capability profile must be consistent across
//! all subsystems (extraction, search, capability reporting).
//!
//! ```text
//! # Run with cangjie feature enabled:
//! cargo test -p atlas-engine --features cangjie --test cangjie_search
//!
//! # Run without cangjie (lang:cj should NOT be recognized):
//! cargo test -p atlas-engine --test cangjie_search
//! ```

use atlas_engine::{self, Language};

// ── Cangjie feature enabled ───────────────────────────────────────────

#[cfg(feature = "cangjie")]
mod cangjie_enabled {
    use super::*;

    /// `lang:cj` (short alias) is recognized by the search query parser.
    #[test]
    fn lang_cj_is_parsed() {
        let query = atlas_engine::parse_query("lang:cj");
        assert_eq!(
            query.language,
            Some(Language::Cangjie),
            "lang:cj should resolve to Cangjie when feature is enabled"
        );
    }

    /// `lang:cangjie` (full name) is recognized.
    #[test]
    fn lang_cangjie_is_parsed() {
        let query = atlas_engine::parse_query("lang:cangjie");
        assert_eq!(
            query.language,
            Some(Language::Cangjie),
            "lang:cangjie should resolve to Cangjie"
        );
    }

    /// `lang:cj` is recognized within a full structured query.
    #[test]
    fn lang_cj_in_structured_query() {
        let query = atlas_engine::parse_query("kind:function lang:cj path:src 搜索");
        assert_eq!(query.kind_filter, Some(atlas_engine::SymbolKind::Function));
        assert_eq!(query.language, Some(Language::Cangjie));
        assert_eq!(query.path_filter.as_deref(), Some("src"));
    }

    /// Cangjie capability profile confirms DataflowFull support.
    #[test]
    fn cangjie_capability_is_dataflow_full() {
        let cap = atlas_engine::Engine::language_capability(Language::Cangjie);
        assert_eq!(
            cap.language, "cangjie",
            "Capability profile language name should be 'cangjie'"
        );
        assert!(
            cap.capability_level >= atlas_engine::CapabilityLevel::DataflowFull,
            "Cangjie should be at least DataflowFull, got {:?}",
            cap.capability_level
        );
    }

    /// Cangjie is included in all_compiled capabilities.
    #[test]
    fn cangjie_in_all_compiled() {
        let caps = atlas_engine::Engine::all_capabilities();
        let has_cangjie = caps.iter().any(|c| c.language == "cangjie");
        assert!(
            has_cangjie,
            "Cangjie should appear in all_compiled() when feature is enabled"
        );
    }

    /// Cangjie frontend is creatable.
    #[test]
    fn cangjie_frontend_creatable() {
        let frontend = atlas_engine::create_frontend(Language::Cangjie);
        assert!(
            frontend.is_some(),
            "Cangjie frontend should be available when feature is enabled"
        );
    }
}

// ── Cangjie feature NOT enabled ───────────────────────────────────────

#[cfg(not(feature = "cangjie"))]
mod cangjie_disabled {
    use super::*;

    /// Without the cangjie feature, `lang:cj` should NOT be recognized.
    #[test]
    fn lang_cj_not_parsed() {
        let query = atlas_engine::parse_query("lang:cj");
        assert_eq!(
            query.language, None,
            "lang:cj should NOT resolve when cangjie feature is disabled"
        );
    }

    /// Without the cangjie feature, `lang:cangjie` should NOT be recognized.
    #[test]
    fn lang_cangjie_not_parsed() {
        let query = atlas_engine::parse_query("lang:cangjie");
        assert_eq!(
            query.language, None,
            "lang:cangjie should NOT resolve when cangjie feature is disabled"
        );
    }

    /// Cangjie frontend should NOT be creatable without the feature.
    #[test]
    fn cangjie_frontend_not_available() {
        let frontend = atlas_engine::create_frontend(Language::Cangjie);
        assert!(
            frontend.is_none(),
            "Cangjie frontend should NOT be available without the feature"
        );
    }
}
