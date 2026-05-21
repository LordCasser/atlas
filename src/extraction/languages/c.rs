//! C LanguageAdapter.

use crate::extraction::languages::{LanguageAdapter, node_range, node_text};
use crate::types::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// C LanguageAdapter.
pub struct CAdapter;

impl LanguageAdapter for CAdapter {
    fn language(&self) -> Language {
        Language::C
    }

    fn extensions(&self) -> &[&str] {
        &["c", "h"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_c::LANGUAGE.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../queries/c/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../queries/c/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../queries/c/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../queries/c/scopes.scm")
    }

    fn normalize_definition(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<SymbolDef> {
        use super::shared::SymbolDefBuilder;

        let kind = c_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);

        let qualified_name = qualified_name_from_node_c(&name, node, source);
        let lang = self.language();
        let signature = c_extract_signature(capture_name, node, source);

        Some(
            SymbolDefBuilder::new(file_id, lang, kind, name, qualified_name, range)
                .signature(signature)
                .build(),
        )
    }

    fn normalize_reference(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<ReferenceUse> {
        let kind = c_reference_kind(capture_name)?;
        let text = node_text(node, source)?;
        let name = text.clone();
        let range = node_range(node);

        let ref_id = ReferenceId::generate(
            &file_id,
            None::<&SymbolId>,
            range.start_byte,
            range.end_byte,
            &text,
            kind,
        );

        // source_symbol is resolved by SemanticBinder after extraction.
        Some(ReferenceUse {
            id: ref_id,
            file_id,
            source_symbol: None,
            scope_id: None,
            kind,
            text,
            name,
            receiver: None,
            arity: None,
            range,
            resolved: None,
            binding_id: None,
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
        let (kind, module, imported_name) = c_import_info(capture_name, node, source)?;
        let range = node_range(node);
        let is_relative = !module.starts_with('<');

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
            local_name: None,
            is_wildcard: false,
            is_relative,
            range,
            alias: None,
        })
    }

    fn normalize_scope(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<ScopeDef> {
        let kind = c_scope_kind(capture_name)?;
        let name = node_text(node, source).unwrap_or_default();
        let range = node_range(node);

        let scope_id =
            ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);

        Some(ScopeDef {
            id: scope_id,
            file_id,
            kind,
            name,
            scope_path: String::new(),
            parent_id: None,
            range,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a C symbol.
fn qualified_name_from_node_c(name: &str, node: tree_sitter::Node, source: &str) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "struct_specifier" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(struct_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(struct_name.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }

    parts.reverse();
    parts.join(".")
}

/// Map capture name to SymbolKind.
fn c_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.class" => Some(SymbolKind::Class), // struct
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.type_alias" => Some(SymbolKind::TypeAlias), // typedef
        "definition.macro" => Some(SymbolKind::Macro),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn c_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

/// Map capture name to ScopeKind.
fn c_scope_kind(capture: &str) -> Option<ScopeKind> {
    match capture {
        "scope.file" => Some(ScopeKind::File),
        "scope.function" => Some(ScopeKind::Function),
        "scope.class" => Some(ScopeKind::Class),
        "scope.block" => Some(ScopeKind::Block),
        "scope.conditional" => Some(ScopeKind::Conditional),
        "scope.loop" => Some(ScopeKind::Loop),
        _ => None,
    }
}

/// Extract import info from capture.
fn c_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned, String::new()))
        }
        "import.include" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned, String::new()))
        }
        _ => None,
    }
}

/// Extract function signature (parameter list) from the AST.
///
/// The `node` is the `function_declarator` captured by `@definition.function`.
/// It has a `parameters` child field containing the `parameter_list`.
fn c_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    if capture_name != "definition.function" {
        return None;
    }
    let params = node.child_by_field_name("parameters")?;
    Some(node_text(params, source)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let adapter = CAdapter;
        assert_eq!(adapter.language(), Language::C);
        assert!(adapter.extensions().contains(&"c"));
        assert!(!adapter.definition_query().is_empty());
    }

    #[test]
    fn test_def_query_parses() {
        let adapter = CAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(
            query.is_ok(),
            "definition query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_ref_query_parses() {
        let adapter = CAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.reference_query());
        assert!(
            query.is_ok(),
            "reference query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_import_query_parses() {
        let adapter = CAdapter;
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
        let adapter = CAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }
}
