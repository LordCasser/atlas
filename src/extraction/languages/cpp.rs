//! C++ LanguageAdapter.

use crate::extraction::languages::{node_range, node_text, LanguageAdapter};
use crate::types::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// C++ LanguageAdapter.
pub struct CppAdapter;

impl LanguageAdapter for CppAdapter {
    fn language(&self) -> Language {
        Language::Cpp
    }

    fn extensions(&self) -> &[&str] {
        &["cpp", "cxx", "cc", "hpp", "hxx", "hh", "h"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../queries/cpp/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../queries/cpp/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../queries/cpp/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../queries/cpp/scopes.scm")
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

        let kind = cpp_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);

        let qualified_name = qualified_name_from_node_cpp(&name, node, source);
        let lang = self.language();
        let signature = cpp_extract_signature(capture_name, node, source);

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
        let kind = cpp_reference_kind(capture_name)?;
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
        let (kind, module, imported_name) = cpp_import_info(capture_name, node, source)?;
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
        let kind = cpp_scope_kind(capture_name)?;
        let name = node_text(node, source).unwrap_or_default();
        let range = node_range(node);

        let scope_id = ScopeId::generate(
            &file_id,
            None::<&ScopeId>,
            kind.as_str(),
            range.start_byte,
        );

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

    fn dataflow_query(&self) -> &str {
        include_str!("../queries/cpp/dataflow.scm")
    }

    fn normalize_dataflow(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<RawEdge> {
        let kind_str = cpp_dataflow_kind(capture_name)?;
        let kind = EdgeKind::from_str(kind_str).unwrap_or(EdgeKind::Assigns);
        let text = node_text(node, source)?;
        let range = node_range(node);

        // Use a placeholder source; SemanticBinder::resolve_edge_sources()
        // will rewrite it via the location field after extraction.
        let placeholder = SymbolId::generate(
            &file_id,
            "placeholder",
            "",
            "placeholder",
            None::<&str>,
        );
        let target = SymbolId::generate(
            &file_id,
            "dataflow",
            &text,
            kind_str,
            None::<&str>,
        );
        let edge_id = EdgeId::generate(
            &placeholder,
            &target,
            kind_str,
            None::<&ReferenceId>,
            Provenance::TreeSitter.as_str(),
        );
        let mut edge = RawEdge::new(
            edge_id,
            placeholder,
            target,
            kind,
            Confidence::certain(),
            Provenance::TreeSitter,
        );
        edge.location = Some(range);
        Some(edge)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn qualified_name_from_node_cpp(name: &str, node: tree_sitter::Node, source: &str) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_specifier" | "struct_specifier" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(class_name.to_string());
                    }
                }
            }
            "namespace_definition" => {
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
    parts.join("::")
}

fn cpp_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.class" => Some(SymbolKind::Class),
        "definition.namespace" => Some(SymbolKind::Namespace),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.macro" => Some(SymbolKind::Macro),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

fn cpp_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

fn cpp_scope_kind(capture: &str) -> Option<ScopeKind> {
    match capture {
        "scope.file" => Some(ScopeKind::File),
        "scope.function" => Some(ScopeKind::Function),
        "scope.class" => Some(ScopeKind::Class),
        "scope.namespace" => Some(ScopeKind::Namespace),
        "scope.block" => Some(ScopeKind::Block),
        "scope.conditional" => Some(ScopeKind::Conditional),
        "scope.loop" => Some(ScopeKind::Loop),
        _ => None,
    }
}

fn cpp_dataflow_kind(capture: &str) -> Option<&'static str> {
    match capture {
        "dataflow.parameter" => Some("parameter"),
        "dataflow.type" => Some("type_of"),
        "dataflow.return" => Some("returns"),
        "dataflow.assign" => Some("assigns"),
        _ => None,
    }
}

fn cpp_import_info(
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
        "import.name" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Use, String::new(), name))
        }
        _ => None,
    }
}

/// Extract function signature (parameter list) from the AST.
///
/// The `node` is the `function_declarator` captured by `@definition.function`.
/// It has a `parameters` child field containing the `parameter_list`.
fn cpp_extract_signature(
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
        let adapter = CppAdapter;
        assert_eq!(adapter.language(), Language::Cpp);
        assert!(adapter.extensions().contains(&"cpp"));
        assert!(adapter.extensions().contains(&"hpp"));
    }

    #[test]
    fn test_def_query_parses() {
        let adapter = CppAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(query.is_ok(), "definition query must compile: {:?}", query.err());
    }

    #[test]
    fn test_ref_query_parses() {
        let adapter = CppAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.reference_query());
        assert!(query.is_ok(), "reference query must compile: {:?}", query.err());
    }

    #[test]
    fn test_import_query_parses() {
        let adapter = CppAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.import_query());
        assert!(query.is_ok(), "import query must compile: {:?}", query.err());
    }

    #[test]
    fn test_scope_query_parses() {
        let adapter = CppAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }
}
