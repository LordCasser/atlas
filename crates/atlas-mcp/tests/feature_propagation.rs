//! Feature propagation tests for atlas-mcp.
//!
//! These tests verify that the MCP crate's feature flags correctly propagate
//! to the atlas-engine dependency.  The conditional compilation tests below
//! are designed to be run with different `--features` / `--no-default-features`
//! flags:
//!
//! ```text
//! # Default build (TS/JS/Python enabled)
//! cargo test -p atlas-mcp
//!
//! # Zero-language build (no default features)
//! cargo test -p atlas-mcp --no-default-features
//!
//! # Cangjie enabled
//! cargo test -p atlas-mcp --no-default-features --features cangjie
//!
//! # All languages
//! cargo test -p atlas-mcp --features all-languages
//! ```
//!
//! ## Verifying zero-language builds
//!
//! For a more thorough check that zero-language builds don't compile any
//! tree-sitter parsers:
//!
//! ```text
//! cargo tree -p atlas-mcp --no-default-features | grep tree-sitter
//! # Expected: no output (tree-sitter parser crates are not in the dep tree)
//! ```

// ── Default build tests (TS/JS/Python) ─────────────────────────────────

#[cfg(all(feature = "typescript", feature = "javascript", feature = "python"))]
mod default_build {
    /// When built with default features, all three core languages are enabled.
    #[test]
    fn core_languages_enabled() {
        assert!(
            cfg!(feature = "typescript"),
            "typescript feature should be enabled"
        );
        assert!(
            cfg!(feature = "javascript"),
            "javascript feature should be enabled"
        );
        assert!(cfg!(feature = "python"), "python feature should be enabled");
    }

    /// The engine's frontend should be creatable for TypeScript.
    #[test]
    fn typescript_frontend_available() {
        let frontend = atlas_engine::create_frontend(atlas_engine::Language::TypeScript);
        assert!(
            frontend.is_some(),
            "TypeScript frontend should be available with default features"
        );
    }

    /// The engine's frontend should be creatable for JavaScript.
    #[test]
    fn javascript_frontend_available() {
        let frontend = atlas_engine::create_frontend(atlas_engine::Language::JavaScript);
        assert!(
            frontend.is_some(),
            "JavaScript frontend should be available with default features"
        );
    }

    /// The engine's frontend should be creatable for Python.
    #[test]
    fn python_frontend_available() {
        let frontend = atlas_engine::create_frontend(atlas_engine::Language::Python);
        assert!(
            frontend.is_some(),
            "Python frontend should be available with default features"
        );
    }
}

// ── Zero-language build tests (--no-default-features) ─────────────────

#[cfg(not(any(
    feature = "typescript",
    feature = "javascript",
    feature = "python",
    feature = "java",
    feature = "c",
    feature = "cpp",
    feature = "arkts",
    feature = "cangjie",
    feature = "go",
    feature = "csharp",
    feature = "rust",
    feature = "php",
    feature = "ruby",
    feature = "kotlin",
)))]
mod zero_language_build {
    #[test]
    fn no_language_features_enabled() {
        assert!(
            !cfg!(feature = "typescript"),
            "typescript should be disabled"
        );
        assert!(
            !cfg!(feature = "javascript"),
            "javascript should be disabled"
        );
        assert!(!cfg!(feature = "python"), "python should be disabled");
        assert!(!cfg!(feature = "cangjie"), "cangjie should be disabled");
    }

    /// In a zero-language build, no frontend should be creatable.
    #[test]
    fn no_frontends_available() {
        assert!(
            atlas_engine::create_frontend(atlas_engine::Language::TypeScript).is_none(),
            "TypeScript frontend should NOT be available without language features"
        );
        assert!(
            atlas_engine::create_frontend(atlas_engine::Language::Python).is_none(),
            "Python frontend should NOT be available without language features"
        );
        assert!(
            atlas_engine::create_frontend(atlas_engine::Language::Cangjie).is_none(),
            "Cangjie frontend should NOT be available without language features"
        );
    }

    /// The engine's all_capabilities should return an empty list in
    /// a zero-language build (no languages compiled in).
    #[test]
    fn all_capabilities_empty() {
        let caps = atlas_engine::Engine::all_capabilities();
        assert!(
            caps.is_empty(),
            "Zero-language build should have no compiled-in language capabilities, got {}",
            caps.len()
        );
    }
}

// ── Cangjie feature tests (--features cangjie) ────────────────────────

#[cfg(feature = "cangjie")]
mod cangjie_build {
    #[test]
    fn cangjie_feature_enabled() {
        assert!(
            cfg!(feature = "cangjie"),
            "cangjie feature should be enabled"
        );
    }

    /// Cangjie frontend should be available.
    #[test]
    fn cangjie_frontend_available() {
        let frontend = atlas_engine::create_frontend(atlas_engine::Language::Cangjie);
        assert!(
            frontend.is_some(),
            "Cangjie frontend should be available with cangjie feature"
        );
    }

    /// The search parser should recognize `lang:cj` when cangjie feature is enabled.
    #[test]
    fn lang_cj_recognized_in_search() {
        let query = atlas_engine::parse_query("lang:cj");
        assert_eq!(
            query.language,
            Some(atlas_engine::Language::Cangjie),
            "lang:cj should resolve to Cangjie when cangjie feature is enabled"
        );
    }

    /// `lang:cangjie` (full name) should also work.
    #[test]
    fn lang_cangjie_recognized_in_search() {
        let query = atlas_engine::parse_query("lang:cangjie");
        assert_eq!(
            query.language,
            Some(atlas_engine::Language::Cangjie),
            "lang:cangjie should resolve to Cangjie"
        );
    }
}

// ── Cangjie-not-enabled tests (without cangjie feature) ───────────────

#[cfg(not(feature = "cangjie"))]
mod cangjie_disabled {
    /// When cangjie is NOT enabled, `lang:cj` should NOT be recognized.
    #[test]
    fn lang_cj_not_recognized_without_cangjie() {
        let query = atlas_engine::parse_query("lang:cj");
        assert_eq!(
            query.language, None,
            "lang:cj should NOT resolve when cangjie feature is disabled"
        );
    }
}
