//! Language registry: loads tree-sitter grammars for enabled Atlas languages.
//!
//! The MVP compile set excludes incomplete/experimental languages such as
//! Cangjie. Those languages must be enabled explicitly with their own feature.

use crate::types::Language;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::Path;

/// Registry of loaded tree-sitter grammars, keyed by Language.
pub struct LanguageRegistry {
    grammars: HashMap<Language, tree_sitter::Language>,
}

impl LanguageRegistry {
    /// Create a new registry and load grammars for the given languages.
    pub fn new(languages: &[Language]) -> Result<Self> {
        let mut registry = Self {
            grammars: HashMap::new(),
        };
        for &lang in languages {
            registry.load_grammar(lang)?;
        }
        Ok(registry)
    }

    /// Get the tree-sitter Language for a given Atlas Language.
    pub fn get(&self, lang: Language) -> Option<&tree_sitter::Language> {
        self.grammars.get(&lang)
    }

    /// Check if a language is loaded.
    pub fn has(&self, lang: Language) -> bool {
        self.grammars.contains_key(&lang)
    }

    /// List loaded languages.
    pub fn loaded_languages(&self) -> Vec<Language> {
        self.grammars.keys().copied().collect()
    }

    /// Detect language from a file path.
    pub fn detect_language(path: &Path) -> Option<Language> {
        Language::from_path(path)
    }

    // --- internal ---

    fn register(&mut self, lang: tree_sitter::Language, atlas_lang: Language) {
        self.grammars.insert(atlas_lang, lang);
    }

    fn load_grammar(&mut self, lang: Language) -> Result<()> {
        match lang {
            #[cfg(feature = "typescript")]
            Language::TypeScript | Language::JavaScript => {
                let ts_lang: tree_sitter::Language =
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
                self.register(ts_lang.clone(), Language::TypeScript);
                self.register(ts_lang, Language::JavaScript);
            }
            #[cfg(feature = "python")]
            Language::Python => {
                let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
                self.register(lang, Language::Python);
            }
            #[cfg(feature = "java")]
            Language::Java => {
                let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
                self.register(lang, Language::Java);
            }
            #[cfg(feature = "c")]
            Language::C => {
                let lang: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
                self.register(lang, Language::C);
            }
            #[cfg(feature = "cpp")]
            Language::Cpp => {
                let lang: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
                self.register(lang, Language::Cpp);
            }
            #[cfg(feature = "arkts")]
            Language::ArkTS => {
                // ArkTS uses TypeScript grammar -> same crate
                let lang: tree_sitter::Language =
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
                self.register(lang, Language::ArkTS);
            }
            #[cfg(feature = "cangjie")]
            Language::Cangjie => {
                let lang: tree_sitter::Language = tree_sitter_cangjie::LANGUAGE.into();
                self.register(lang, Language::Cangjie);
            }
            _ => bail!(
                "Language {:?} not enabled (missing feature flag or not yet implemented)",
                lang
            ),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("src/main.ts")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("src/main.js")),
            Some(Language::JavaScript)
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("src/main.py")),
            Some(Language::Python)
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("App.ets")),
            Some(Language::ArkTS)
        );
        #[cfg(feature = "cangjie")]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("hello.cj")),
            Some(Language::Cangjie)
        );
        #[cfg(not(feature = "cangjie"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("hello.cj")),
            None
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("Main.java")),
            Some(Language::Java)
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.c")),
            Some(Language::C)
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.cpp")),
            Some(Language::Cpp)
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("unknown.xyz")),
            None
        );
        // Non-MVP languages
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.go")),
            None
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.rs")),
            None
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_load_typescript() {
        let registry = LanguageRegistry::new(&[Language::TypeScript]).unwrap();
        assert!(registry.has(Language::TypeScript));
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_load_javascript() {
        // JavaScript uses TypeScript grammar — must be loadable independently
        let registry = LanguageRegistry::new(&[Language::JavaScript]).unwrap();
        assert!(registry.has(Language::JavaScript));
        assert!(registry.has(Language::TypeScript));
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_load_typescript_javascript_both() {
        let registry =
            LanguageRegistry::new(&[Language::TypeScript, Language::JavaScript]).unwrap();
        assert!(registry.has(Language::TypeScript));
        assert!(registry.has(Language::JavaScript));
    }

    #[cfg(feature = "python")]
    #[test]
    fn test_load_python() {
        let registry = LanguageRegistry::new(&[Language::Python]).unwrap();
        assert!(registry.has(Language::Python));
    }

    #[cfg(feature = "arkts")]
    #[test]
    fn test_load_arkts() {
        let registry = LanguageRegistry::new(&[Language::ArkTS]).unwrap();
        assert!(registry.has(Language::ArkTS));
    }
}
