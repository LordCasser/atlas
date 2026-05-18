//! TypeScript / JavaScript LanguageAdapter implementation.
//!
//! Uses tree-sitter-typescript grammar and embedded query files.
//! JavaScript is treated as a subset of TypeScript for extraction purposes.

use crate::extraction::languages::LanguageAdapter;
use crate::types::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// TypeScript LanguageAdapter (also covers JavaScript).
pub struct TypeScriptAdapter;

impl LanguageAdapter for TypeScriptAdapter {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn extensions(&self) -> &[&str] {
        &["ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx"]
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
        _file_path: &Path,
    ) -> Option<SymbolDef> {
        let kind = ts_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);
        let name_range = node_range(node); // The captured node IS the name node in TS queries

        let qualified_name = qualified_name_from_node("", &name, node, source);
        let lang = self.language();

        let symbol_id = SymbolId::generate(
            &file_id,
            lang.as_str(),
            &qualified_name,
            kind.as_str(),
            None::<&str>,
        );

        Some(SymbolDef {
            id: symbol_id,
            kind,
            name,
            qualified_name,
            symbol_path: vec![],
            file_id,
            language: lang,
            range,
            name_range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
        })
    }

    fn normalize_reference(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<ReferenceUse> {
        let kind = ts_reference_kind(capture_name)?;
        let text = node_text(node, source)?;
        let name = text.clone();
        let range = node_range(node);

        let ref_id = ReferenceId::generate(
            &file_id,
            None::<&SymbolId>,
            range.start_byte,
            range.end_byte,
            &text,
        );

        Some(ReferenceUse {
            id: ref_id,
            file_id,
            source_symbol: None, // Filled by the resolver during scope analysis
            scope_id: None,
            kind,
            text,
            name,
            receiver: None,
            arity: None,
            range,
            resolved: None,
        })
    }

    fn normalize_import(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<ImportDef> {
        let (kind, module, imported_name) = ts_import_info(capture_name, node, source)?;
        let range = node_range(node);
        let local_name = imported_name.clone();
        let is_relative = module.starts_with('.');
        let is_wildcard = capture_name.contains("wildcard");

        let import_id = ImportId::generate(
            &file_id,
            kind.as_str(),
            &module,
            Some(imported_name.as_str()),
            range.start_byte,
        );

        Some(ImportDef {
            id: import_id,
            file_id,
            kind,
            module,
            imported_name,
            local_name: Some(local_name),
            is_wildcard,
            is_relative,
            range,
            alias: None,
        })
    }

    fn detect_package(&self, _source: &str, file_path: &Path) -> Option<String> {
        // For TypeScript, check package.json in parent directories
        let mut current = file_path.parent()?;
        loop {
            let pkg_json = current.join("package.json");
            if let Ok(content) = std::fs::read_to_string(&pkg_json) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(name) = json["name"].as_str() {
                        return Some(name.to_string());
                    }
                }
            }
            current = current.parent()?;
        }
    }

    fn detect_frameworks(&self, source: &str) -> Vec<String> {
        let mut frameworks = Vec::new();
        // Quick heuristic detection based on import patterns
        if source.contains("react") {
            frameworks.push("react".into());
        }
        if source.contains("@angular") {
            frameworks.push("angular".into());
        }
        if source.contains("vue") {
            frameworks.push("vue".into());
        }
        if source.contains("express") {
            frameworks.push("express".into());
        }
        if source.contains("next") {
            frameworks.push("next".into());
        }
        frameworks
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract text content from a tree-sitter node.
fn node_text(node: tree_sitter::Node, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes()).ok().map(|s| s.to_string())
}

/// Build a TextRange from a tree-sitter node.
fn node_range(node: tree_sitter::Node) -> TextRange {
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

/// Infer a qualified name from node's parent hierarchy.
fn qualified_name_from_node(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node;

    // Walk up parent scopes to build qualified name
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "class" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(class_name.to_string());
                    }
                }
            }
            "namespace_declaration" | "module" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(ns_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(ns_name.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }

    parts.reverse();
    let prefix_str = if prefix.is_empty() { "" } else { prefix };
    if prefix_str.is_empty() {
        parts.join(".")
    } else {
        format!("{}.{}", prefix_str, parts.join("."))
    }
}

/// Map capture name to SymbolKind.
fn ts_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.type_alias" => Some(SymbolKind::TypeAlias),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn ts_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.extends" => Some(ReferenceKind::Inheritance),
        "reference.implements" => Some(ReferenceKind::Implementation),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.usage" => Some(ReferenceKind::Usage),
        _ => None,
    }
}

/// Extract import info from capture.
fn ts_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let module_path = node_text(node, source)?;
            let cleaned = module_path.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Import, cleaned, String::new()))
        }
        "import.name" | "import.alias" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Import, String::new(), name))
        }
        "import.namespace" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Import, String::new(), name))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let adapter = TypeScriptAdapter;
        assert_eq!(adapter.language(), Language::TypeScript);
        assert!(adapter.extensions().contains(&"ts"));
        assert!(adapter.extensions().contains(&"js"));
        assert!(!adapter.definition_query().is_empty());
        assert!(!adapter.reference_query().is_empty());
        assert!(!adapter.import_query().is_empty());
        assert!(!adapter.scope_query().is_empty());
    }

    #[test]
    fn test_def_query_parses() {
        let adapter = TypeScriptAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(query.is_ok(), "definition query must compile");
    }

    #[test]
    fn test_ref_query_parses() {
        let adapter = TypeScriptAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.reference_query());
        assert!(query.is_ok(), "reference query must compile");
    }

    #[test]
    fn test_import_query_parses() {
        let adapter = TypeScriptAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.import_query());
        assert!(
            query.is_ok(),
            "import query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_scope_query_parses() {
        let adapter = TypeScriptAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.scope_query());
        assert!(
            query.is_ok(),
            "scope query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_ts_definition_kind_mapping() {
        assert_eq!(
            ts_definition_kind("definition.function"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            ts_definition_kind("definition.class"),
            Some(SymbolKind::Class)
        );
        assert_eq!(ts_definition_kind("unknown.capture"), None);
    }

    #[test]
    fn test_ts_reference_kind_mapping() {
        assert_eq!(
            ts_reference_kind("reference.call"),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            ts_reference_kind("reference.field"),
            Some(ReferenceKind::FieldAccess)
        );
        assert_eq!(ts_reference_kind("unknown.capture"), None);
    }
}
