//! LanguageAdapter trait — the per-language query-driven extractor interface.
//!
//! ## Design
//! - One `LanguageAdapter` implementation per language.
//! - Each adapter provides tree-sitter queries for definitions, references, imports, scopes.
//! - The `normalize_*` methods convert raw query captures into Atlas IR types.
//! - Extraction never writes final edges — that belongs to the resolution phase.
//!
//! ## Adding a new language
//! 1. Create `atlas-languages/src/<lang>.rs`
//! 2. Add tree-sitter query files in `atlas-languages/queries/<lang>/`
//! 3. Implement `LanguageAdapter` trait
//! 4. Feature-gate with `#[cfg(feature = "<lang>")]`

use crate::types::*;
use std::path::Path;

/// Per-language extraction adapter. Query-driven, not AST-walker-driven.
pub trait LanguageAdapter: Send + Sync {
    // -------------------------------------------------------------------
    // Language identity
    // -------------------------------------------------------------------

    /// The Language variant this adapter handles.
    fn language(&self) -> Language;

    /// File extensions this language uses (e.g. &["ts", "mts", "cts"]).
    fn extensions(&self) -> &[&str];

    /// The tree-sitter Language grammar for parsing.
    fn tree_sitter_language(&self) -> tree_sitter::Language;

    // -------------------------------------------------------------------
    // Tree-sitter query strings
    // -------------------------------------------------------------------

    /// S-expression query for symbol definitions.
    fn definition_query(&self) -> &str;

    /// S-expression query for reference uses.
    fn reference_query(&self) -> &str;

    /// S-expression query for import statements.
    fn import_query(&self) -> &str;

    /// S-expression query for scopes (containment regions).
    fn scope_query(&self) -> &str;

    // -------------------------------------------------------------------
    // Normalization: raw capture → Atlas IR
    // -------------------------------------------------------------------

    /// Convert a tree-sitter query capture into a `SymbolDef`, or `None`
    /// if the capture doesn't represent a valid definition.
    fn normalize_definition(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<SymbolDef>;

    /// Convert a query capture into a `ReferenceUse`, or `None`.
    fn normalize_reference(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<ReferenceUse>;

    /// Convert a query capture into an `ImportDef`, or `None`.
    fn normalize_import(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<ImportDef>;

    // -------------------------------------------------------------------
    // Optional hooks
    // -------------------------------------------------------------------

    /// Detect package name / module name from the source file.
    fn detect_package(&self, _source: &str, _file_path: &Path) -> Option<String> {
        None
    }

    /// Detect frameworks used in the source file.
    fn detect_frameworks(&self, _source: &str) -> Vec<String> {
        Vec::new()
    }
}

// Marker: this module intentionally has NO default per-language implementations.
// Language adapters live in `atlas-languages/` crate or `extraction/languages/<lang>.rs`.

// Language-specific adapters will be added in subsequent milestones:
// #[cfg(feature = "typescript")]
// mod typescript;
// #[cfg(feature = "python")]
// mod python;
// #[cfg(feature = "java")]
// mod java;
// #[cfg(feature = "c")]
// mod c;
// #[cfg(feature = "cpp")]
// mod cpp;
// #[cfg(feature = "arkts")]
// mod arkts;
// #[cfg(feature = "cangjie")]
// mod cangjie;
