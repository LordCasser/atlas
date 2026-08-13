//! Language frontend specs — per-language slot-based extractor implementations.
//!
//! ## Design
//! - One frontend spec struct per language.
//! - Each spec implements the slot traits from `crate::frontend` (ParserSpec,
//!   SymbolExtractorSpec, ReferenceExtractorSpec, etc.).
//! - The slot traits return tree-sitter queries and normalize raw captures into
//!   Atlas IR types.
//! - Extraction never writes final edges — that belongs to the resolution phase.
//!
//! ## Adding a new language
//! 1. Create `src/extraction/languages/<lang>.rs`
//! 2. Add tree-sitter query files in `src/extraction/queries/<lang>/`
//! 3. Implement the relevant slot traits (ParserSpec, SymbolExtractorSpec, etc.)
//! 4. Feature-gate with `#[cfg(feature = "<lang>")]`
//! 5. Add a `*_frontend()` factory function

use types::*;

pub mod shared;

// ── Shared helpers (used by all language frontend specs) ────────────────

/// Extract the UTF-8 text of a tree-sitter node from the source string.
pub fn node_text(node: tree_sitter::Node, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes())
        .ok()
        .map(|s| s.to_string())
}

/// Build a `TextRange` from a tree-sitter node's byte and line/column positions.
pub fn node_range(node: tree_sitter::Node) -> TextRange {
    let start = node.start_position();
    let end = node.end_position();
    TextRange {
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        start_line: start.row as u32,
        start_column: start.column as u32,
        end_line: end.row as u32,
        end_column: end.column as u32,
    }
}

// Language-specific frontend specs — feature-gated per language.

#[cfg(any(feature = "typescript", feature = "javascript", feature = "arkts"))]
pub mod typescript;

#[cfg(feature = "javascript")]
pub mod javascript;

#[cfg(feature = "python")]
pub mod python;

#[cfg(feature = "arkts")]
pub mod arkts;

#[cfg(feature = "java")]
pub mod java;

#[cfg(feature = "c")]
pub mod c;

#[cfg(feature = "cpp")]
pub mod cpp;

#[cfg(feature = "cangjie")]
pub mod cangjie;

#[cfg(feature = "go")]
pub mod go;

#[cfg(feature = "csharp")]
pub mod csharp;

#[cfg(feature = "rust")]
pub mod rust;

#[cfg(feature = "php")]
pub mod php;

#[cfg(feature = "ruby")]
pub mod ruby;

#[cfg(feature = "kotlin")]
pub mod kotlin;

/// Create a `LanguageFrontend` for the given language.
/// Returns `None` if the language's frontend is not compiled in (feature-gated).
///
/// `LanguageFrontend` is the slot-based interface for typed feature queries.
/// Each language is built via direct slot construction (see `*_frontend()` factories).
pub fn create_frontend(lang: Language) -> Option<crate::frontend::LanguageFrontend> {
    match lang {
        #[cfg(feature = "typescript")]
        Language::TypeScript => Some(typescript::typescript_frontend()),
        #[cfg(feature = "javascript")]
        Language::JavaScript => Some(javascript::javascript_frontend()),
        #[cfg(feature = "python")]
        Language::Python => Some(python::python_frontend()),
        #[cfg(feature = "arkts")]
        Language::ArkTS => Some(arkts::arkts_frontend()),
        #[cfg(feature = "java")]
        Language::Java => Some(java::java_frontend()),
        #[cfg(feature = "c")]
        Language::C => Some(c::c_frontend()),
        #[cfg(feature = "cpp")]
        Language::Cpp => Some(cpp::cpp_frontend()),
        #[cfg(feature = "cangjie")]
        Language::Cangjie => Some(cangjie::cangjie_frontend()),
        #[cfg(feature = "go")]
        Language::Go => Some(go::go_frontend()),
        #[cfg(feature = "csharp")]
        Language::CSharp => Some(csharp::csharp_frontend()),
        #[cfg(feature = "rust")]
        Language::Rust => Some(rust::rust_frontend()),
        #[cfg(feature = "php")]
        Language::Php => Some(php::php_frontend()),
        #[cfg(feature = "ruby")]
        Language::Ruby => Some(ruby::ruby_frontend()),
        #[cfg(feature = "kotlin")]
        Language::Kotlin => Some(kotlin::kotlin_frontend()),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Languages whose frontend can be created at runtime (subset of
/// [`Language::enabled_languages`] where the tree-sitter grammar was
/// successfully loaded).
pub fn available_languages() -> Vec<Language> {
    Language::enabled_languages()
        .into_iter()
        .filter(|&l| create_frontend(l).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enabled_language_has_a_frontend() {
        for language in Language::enabled_languages() {
            assert!(
                create_frontend(language).is_some(),
                "{} is enabled but has no extraction frontend",
                language.as_str()
            );
        }
    }
}
