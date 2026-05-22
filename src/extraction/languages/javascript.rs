//! JavaScript LanguageAdapter — thin wrapper around TypeScript.
//!
//! JavaScript shares TypeScript's tree-sitter grammar and query files,
//! but reports a distinct Language::JavaScript identity so database
//! records reflect the actual source language of each file.
//!
//! The adapter delegates all normalization to `TypeScriptAdapter`,
//! only overriding `language()` and `extensions()`.

use crate::extraction::languages::LanguageAdapter;
use crate::types::*;
use std::path::Path;

/// JavaScript adapter — delegates to TypeScript internally.
pub struct JavaScriptAdapter;

impl LanguageAdapter for JavaScriptAdapter {
    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn extensions(&self) -> &[&str] {
        &["js", "mjs", "cjs"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../queries/typescript/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../queries/typescript/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../queries/typescript/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../queries/typescript/scopes.scm")
    }

    fn normalize_definition(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<SymbolDef> {
        let mut def = super::typescript::TypeScriptAdapter.normalize_definition(
            capture_name,
            node,
            source,
            file_id,
            file_path,
        )?;
        def.language = Language::JavaScript;
        Some(def)
    }

    fn normalize_reference(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<ReferenceUse> {
        super::typescript::TypeScriptAdapter.normalize_reference(
            capture_name,
            node,
            source,
            file_id,
            file_path,
        )
    }

    fn normalize_import(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<ImportDef> {
        super::typescript::TypeScriptAdapter.normalize_import(
            capture_name,
            node,
            source,
            file_id,
            file_path,
        )
    }

    fn normalize_scope(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<ScopeDef> {
        super::typescript::TypeScriptAdapter.normalize_scope(
            capture_name,
            node,
            source,
            file_id,
            file_path,
        )
    }

    fn detect_package(&self, source: &str, file_path: &Path) -> Option<String> {
        super::typescript::TypeScriptAdapter.detect_package(source, file_path)
    }

    fn detect_frameworks(&self, source: &str) -> Vec<String> {
        super::typescript::TypeScriptAdapter.detect_frameworks(source)
    }

    fn lexical_query(&self) -> &str {
        super::typescript::TypeScriptAdapter.lexical_query()
    }

    fn dataflow_builder_query(&self) -> &str {
        super::typescript::TypeScriptAdapter.dataflow_builder_query()
    }

    fn normalize_lexical(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> Option<crate::types::bindings::BindingDef> {
        super::typescript::TypeScriptAdapter.normalize_lexical(
            capture_name, node, source, file_id, file_path,
        )
    }

    fn normalize_dataflow_builder(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        file_path: &Path,
    ) -> (
        Option<crate::types::dataflow::DataNode>,
        Option<crate::types::dataflow::DataFlowEdge>,
    ) {
        super::typescript::TypeScriptAdapter.normalize_dataflow_builder(
            capture_name, node, source, file_id, file_path,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_adapter_metadata() {
        let adapter = JavaScriptAdapter;
        assert_eq!(adapter.language(), Language::JavaScript);
        assert!(adapter.extensions().contains(&"js"));
        assert!(adapter.extensions().contains(&"mjs"));
        assert!(!adapter.definition_query().is_empty());
    }
}
