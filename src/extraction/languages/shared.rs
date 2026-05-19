//! Shared helpers for language adapters.
//!
//! ## SymbolDefBuilder
//! Eliminates ~60% code duplication across adapters by standardizing the
//! `SymbolDef` construction pattern. Every adapter's `normalize_definition`
//! follows the same flow: (1) determine kind, (2) compute qualified name,
//! (3) optionally extract signature/exported/name_range, (4) build.
//!
//! The builder handles step (4) — SymbolId generation and default field
//! population — so adapters only express what varies.

use crate::types::*;

/// Builder for `SymbolDef` — standardizes the repetitive construction
/// pattern shared by all language adapters.
#[derive(Debug, Clone)]
pub struct SymbolDefBuilder {
    file_id: FileId,
    language: Language,
    kind: SymbolKind,
    name: String,
    qualified_name: String,
    range: TextRange,
    name_range: Option<TextRange>,
    signature: Option<String>,
    exported: bool,
}

impl SymbolDefBuilder {
    /// Create a new builder with required fields.
    pub fn new(
        file_id: FileId,
        language: Language,
        kind: SymbolKind,
        name: String,
        qualified_name: String,
        range: TextRange,
    ) -> Self {
        Self {
            file_id,
            language,
            kind,
            name,
            qualified_name,
            range,
            name_range: None,
            signature: None,
            exported: false,
        }
    }

    /// Set the name-only range (for precise go-to-definition).
    /// If not set, falls back to `range`.
    pub fn name_range(mut self, r: TextRange) -> Self {
        self.name_range = Some(r);
        self
    }

    /// Set the function/method signature string.
    pub fn signature(mut self, sig: Option<String>) -> Self {
        self.signature = sig;
        self
    }

    /// Set whether the symbol is exported.
    pub fn exported(mut self, exported: bool) -> Self {
        self.exported = exported;
        self
    }

    /// Build the `SymbolDef`.
    ///
    /// Generates a deterministic `SymbolId` from (file_id, language,
    /// qualified_name, kind) via blake3.
    pub fn build(self) -> SymbolDef {
        let symbol_id = SymbolId::generate(
            &self.file_id,
            self.language.as_str(),
            &self.qualified_name,
            self.kind.as_str(),
            None::<&str>,
        );

        SymbolDef {
            id: symbol_id,
            kind: self.kind,
            name: self.name,
            qualified_name: self.qualified_name,
            symbol_path: Vec::new(),
            file_id: self.file_id,
            language: self.language,
            range: self.range,
            name_range: self.name_range.unwrap_or(self.range),
            signature: self.signature,
            visibility: None,
            exported: self.exported,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: Vec::new(),
        }
    }
}

/// Extract the precise range for a name token within a definition node.
///
/// Most adapters currently set `name_range = node_range(node)` which makes
/// go-to-definition highlight the entire declaration. This helper extracts
/// just the name token's range by finding the first child that matches the
/// symbol name text.
pub fn node_name_range(node: tree_sitter::Node, name: &str, source: &str) -> Option<TextRange> {
    // For simple cases where the name is the first identifier child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        let kind = child.kind();
        // Handle common identifier-like node types
        if matches!(kind, "identifier" | "type_identifier" | "property_identifier" | "shorthand_property_identifier") {
            if let Ok(text) = child.utf8_text(source.as_bytes()) {
                if text == name {
                    return Some(super::node_range(child));
                }
            }
        }
    }
    // Fall back to the first identifier child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind().contains("identifier") {
            return Some(super::node_range(child));
        }
    }
    None
}
