//! Language registry: loads tree-sitter grammars for enabled Atlas languages.
//!
//! All languages (including Cangjie) are ABI-compatible with tree-sitter 0.26
//! (MAX_ABI ≥ 15).  Experimental languages like Cangjie remain opt-in
//! at the capability level, but no longer require ABI-version workarounds.

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::Path;
use types::Language;

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
            #[cfg(feature = "go")]
            Language::Go => {
                let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
                self.register(lang, Language::Go);
            }
            #[cfg(feature = "csharp")]
            Language::CSharp => {
                let lang: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
                self.register(lang, Language::CSharp);
            }
            #[cfg(feature = "rust")]
            Language::Rust => {
                let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
                self.register(lang, Language::Rust);
            }
            #[cfg(feature = "php")]
            Language::Php => {
                let lang: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
                self.register(lang, Language::Php);
            }
            #[cfg(feature = "ruby")]
            Language::Ruby => {
                let lang: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
                self.register(lang, Language::Ruby);
            }
            #[cfg(feature = "kotlin")]
            Language::Kotlin => {
                let lang: tree_sitter::Language = tree_sitter_kotlin::LANGUAGE.into();
                self.register(lang, Language::Kotlin);
            }
            #[allow(unreachable_patterns)]
            _ => {
                bail!("Language {lang:?} not enabled (missing feature flag or not yet implemented)",)
            }
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
        #[cfg(feature = "arkts")]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("App.ets")),
            Some(Language::ArkTS)
        );
        #[cfg(not(feature = "arkts"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("App.ets")),
            None
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
        #[cfg(feature = "go")]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.go")),
            Some(Language::Go)
        );
        #[cfg(not(feature = "go"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.go")),
            None
        );
        #[cfg(feature = "java")]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("Main.java")),
            Some(Language::Java)
        );
        #[cfg(not(feature = "java"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("Main.java")),
            None
        );
        #[cfg(feature = "c")]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.c")),
            Some(Language::C)
        );
        #[cfg(not(feature = "c"))]
        assert_eq!(LanguageRegistry::detect_language(Path::new("main.c")), None);
        #[cfg(feature = "cpp")]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.cpp")),
            Some(Language::Cpp)
        );
        #[cfg(not(feature = "cpp"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.cpp")),
            None
        );
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("unknown.xyz")),
            None
        );
        // Non-MVP languages
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.go")),
            Language::from_extension("go")
        );
        #[cfg(not(feature = "rust"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.rs")),
            None
        );
        #[cfg(not(feature = "php"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.php")),
            None
        );
        #[cfg(not(feature = "ruby"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("main.rb")),
            None
        );
        #[cfg(not(feature = "kotlin"))]
        assert_eq!(
            LanguageRegistry::detect_language(Path::new("Main.kt")),
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

    #[cfg(feature = "go")]
    #[test]
    fn test_load_go() {
        let registry = LanguageRegistry::new(&[Language::Go]).unwrap();
        assert!(registry.has(Language::Go));
    }

    #[cfg(feature = "csharp")]
    #[test]
    fn test_load_csharp() {
        let registry = LanguageRegistry::new(&[Language::CSharp]).unwrap();
        assert!(registry.has(Language::CSharp));
    }

    #[cfg(feature = "rust")]
    #[test]
    fn test_load_rust() {
        let registry = LanguageRegistry::new(&[Language::Rust]).unwrap();
        assert!(registry.has(Language::Rust));
    }

    #[cfg(feature = "php")]
    #[test]
    fn test_load_php() {
        let registry = LanguageRegistry::new(&[Language::Php]).unwrap();
        assert!(registry.has(Language::Php));
    }

    #[cfg(feature = "ruby")]
    #[test]
    fn test_load_ruby() {
        let registry = LanguageRegistry::new(&[Language::Ruby]).unwrap();
        assert!(registry.has(Language::Ruby));
    }

    #[cfg(feature = "kotlin")]
    #[test]
    fn test_load_kotlin() {
        let registry = LanguageRegistry::new(&[Language::Kotlin]).unwrap();
        assert!(registry.has(Language::Kotlin));
    }
}
