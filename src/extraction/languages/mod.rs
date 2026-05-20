//! LanguageAdapter trait — the per-language query-driven extractor interface.
//!
//! ## Design
//! - One `LanguageAdapter` implementation per language.
//! - Each adapter provides tree-sitter queries for definitions, references, imports, scopes.
//! - The `normalize_*` methods convert raw query captures into Atlas IR types.
//! - Extraction never writes final edges — that belongs to the resolution phase.
//!
//! ## Adding a new language
//! 1. Create `src/extraction/languages/<lang>.rs`
//! 2. Add tree-sitter query files in `src/extraction/queries/<lang>/`
//! 3. Implement `LanguageAdapter` trait
//! 4. Feature-gate with `#[cfg(feature = "<lang>")]`

use crate::types::*;
use std::path::Path;

pub mod shared;

// ── Shared helpers (used by all language adapters) ──────────────────────

/// Extract the UTF-8 text of a tree-sitter node from the source string.
pub fn node_text(node: tree_sitter::Node, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes()).ok().map(|s| s.to_string())
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

    /// S-expression query for dataflow (parameters, returns, assignments).
    /// Default returns empty — dataflow is optional.
    fn dataflow_query(&self) -> &str {
        ""
    }

    /// S-expression query for lexical binding extraction.
    ///
    /// Captures parameter declarations, local variable declarations (let/const/var),
    /// import aliases, catch variables, and destructuring patterns.
    /// Returns empty string by default — languages opt in via their query files.
    fn lexical_query(&self) -> &str {
        ""
    }

    /// S-expression query for dataflow builder.
    ///
    /// Captures assignments, return statements, call arguments, member access chains,
    /// and literals for per-function dataflow graph construction.
    /// Returns empty string by default — languages opt in via their query files.
    fn dataflow_builder_query(&self) -> &str {
        ""
    }

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

    /// Convert a query capture into a `ScopeDef`, or `None`.
    /// Default implementation returns `None` — scopes are optional for MVP extraction.
    fn normalize_scope(
        &self,
        _capture_name: &str,
        _node: tree_sitter::Node,
        _source: &str,
        _file_id: FileId,
        _file_path: &Path,
    ) -> Option<ScopeDef> {
        None
    }

    /// Convert a dataflow query capture into a `RawEdge` (Parameter, Returns, Assigns,
    /// FieldRead, FieldWrite), or `None`. Default returns `None`.
    fn normalize_dataflow(
        &self,
        _capture_name: &str,
        _node: tree_sitter::Node,
        _source: &str,
        _file_id: FileId,
        _file_path: &Path,
    ) -> Option<RawEdge> {
        None
    }

    /// Convert a lexical query capture into a `BindingDef`, or `None`.
    /// Default returns `None` — language adapters opt in by overriding.
    fn normalize_lexical(
        &self,
        _capture_name: &str,
        _node: tree_sitter::Node,
        _source: &str,
        _file_id: FileId,
        _file_path: &Path,
    ) -> Option<crate::types::bindings::BindingDef> {
        None
    }

    /// Convert a dataflow builder query capture into a `DataNode` or `DataFlowEdge`,
    /// returned as a tuple of optional vectors. Default returns `(None, None)`.
    fn normalize_dataflow_builder(
        &self,
        _capture_name: &str,
        _node: tree_sitter::Node,
        _source: &str,
        _file_id: FileId,
        _file_path: &Path,
    ) -> (Option<crate::types::dataflow::DataNode>, Option<crate::types::dataflow::DataFlowEdge>) {
        (None, None)
    }

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

// Language-specific adapters — feature-gated per language.

#[cfg(feature = "typescript")]
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

/// Create a LanguageAdapter for the given language.
/// Returns `None` if the language's adapter is not compiled in (feature-gated).
pub fn create_adapter(lang: Language) -> Option<Box<dyn LanguageAdapter>> {
    match lang {
        #[cfg(feature = "typescript")]
        Language::TypeScript => Some(Box::new(typescript::TypeScriptAdapter)),
        #[cfg(feature = "javascript")]
        Language::JavaScript => Some(Box::new(javascript::JavaScriptAdapter)),
        #[cfg(feature = "python")]
        Language::Python => Some(Box::new(python::PythonAdapter)),
        #[cfg(feature = "arkts")]
        Language::ArkTS => Some(Box::new(arkts::ArkTsAdapter)),
        #[cfg(feature = "java")]
        Language::Java => Some(Box::new(java::JavaAdapter)),
        #[cfg(feature = "c")]
        Language::C => Some(Box::new(c::CAdapter)),
        #[cfg(feature = "cpp")]
        Language::Cpp => Some(Box::new(cpp::CppAdapter)),
        #[cfg(feature = "cangjie")]
        Language::Cangjie => Some(Box::new(cangjie::CangjieAdapter)),
        _ => None,
    }
}
