//! ArkTS LanguageAdapter — thin wrapper around TypeScript.
//!
//! ArkTS (HarmonyOS) uses TypeScript-compatible syntax with `.ets`/`.sts` extensions.
//! The adapter delegates all normalization to `TypeScriptAdapter`, only overriding
//! `language()` and `extensions()`.

use crate::languages::LanguageAdapter;
use atlas_types::*;
use std::path::Path;

/// ArkTS adapter — delegates to TypeScript internally.
pub struct ArkTsAdapter;

impl LanguageAdapter for ArkTsAdapter {
    fn language(&self) -> Language {
        Language::ArkTS
    }

    fn extensions(&self) -> &[&str] {
        &["ets", "sts"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../../queries/typescript/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../../queries/typescript/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../../queries/typescript/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../../queries/typescript/scopes.scm")
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
        def.language = Language::ArkTS;
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
        // ArkTS uses the same package.json convention
        super::typescript::TypeScriptAdapter.detect_package(source, file_path)
    }

    fn detect_frameworks(&self, source: &str) -> Vec<String> {
        let mut frameworks = super::typescript::TypeScriptAdapter.detect_frameworks(source);
        // Add ArkTS-specific framework detection
        if source.contains("@ohos") || source.contains("arkui") {
            frameworks.push("harmonyos".into());
        }
        frameworks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arkts_adapter_metadata() {
        let adapter = ArkTsAdapter;
        assert_eq!(adapter.language(), Language::ArkTS);
        assert!(adapter.extensions().contains(&"ets"));
        assert!(adapter.extensions().contains(&"sts"));
        assert!(!adapter.definition_query().is_empty());
    }

    #[test]
    fn test_arkts_def_query_parses() {
        let adapter = ArkTsAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(query.is_ok(), "definition query must compile");
    }

    #[test]
    fn test_arkts_scope_query_parses() {
        let adapter = ArkTsAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }
}
