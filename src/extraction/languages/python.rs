//! Python LanguageAdapter implementation.
//!
//! Uses tree-sitter-python grammar and embedded query files.

use crate::extraction::languages::LanguageAdapter;
use crate::types::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Python LanguageAdapter.
pub struct PythonAdapter;

impl LanguageAdapter for PythonAdapter {
    fn language(&self) -> Language {
        Language::Python
    }

    fn extensions(&self) -> &[&str] {
        &["py", "pyi"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../queries/python/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../queries/python/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../queries/python/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../queries/python/scopes.scm")
    }

    fn normalize_definition(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<SymbolDef> {
        let kind = py_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);
        let name_range = node_range(node);

        let qualified_name = qualified_name_from_node_py("", &name, node, source);
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
        let kind = py_reference_kind(capture_name)?;
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
        let (kind, module, imported_name, is_relative) =
            py_import_info(capture_name, node, source)?;
        let range = node_range(node);
        let local_name = imported_name.clone();
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

    fn normalize_scope(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        _source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<ScopeDef> {
        let kind = match capture_name {
            "scope.file" => ScopeKind::File,
            "scope.function" => ScopeKind::Function,
            "scope.class" => ScopeKind::Class,
            "scope.block" => ScopeKind::Block,
            _ => return None,
        };
        let range = node_range(node);
        let name = format!("{:?}#{}", kind, range.start_byte);
        let scope_path = name.clone();

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
            scope_path,
            parent_id: None,
            range,
        })
    }

    fn detect_package(&self, _source: &str, file_path: &Path) -> Option<String> {
        // For Python, look for setup.py, pyproject.toml, or __init__.py
        let mut current = file_path.parent()?;
        loop {
            // Check for pyproject.toml
            let pyproject = current.join("pyproject.toml");
            if let Ok(content) = std::fs::read_to_string(&pyproject) {
                if let Some(name) = extract_toml_project_name(&content) {
                    return Some(name);
                }
            }
            // Check for setup.cfg
            let setup_cfg = current.join("setup.cfg");
            if let Ok(content) = std::fs::read_to_string(&setup_cfg) {
                for line in content.lines() {
                    if let Some(name) = line.strip_prefix("name = ") {
                        return Some(name.trim().to_string());
                    }
                }
            }
            // If we found __init__.py, this is a package
            if current.join("__init__.py").is_file() {
                if let Some(name) = current.file_name().and_then(|n| n.to_str()) {
                    return Some(name.to_string());
                }
            }
            current = current.parent()?;
        }
    }

    fn detect_frameworks(&self, source: &str) -> Vec<String> {
        let mut frameworks = Vec::new();
        if source.contains("django") {
            frameworks.push("django".into());
        }
        if source.contains("flask") {
            frameworks.push("flask".into());
        }
        if source.contains("fastapi") {
            frameworks.push("fastapi".into());
        }
        if source.contains("sqlalchemy") {
            frameworks.push("sqlalchemy".into());
        }
        if source.contains("pytest") {
            frameworks.push("pytest".into());
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

/// Infer a qualified name for a Python symbol.
fn qualified_name_from_node_py(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node;

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_definition" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(class_name.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }

    parts.reverse();
    if prefix.is_empty() {
        parts.join(".")
    } else {
        format!("{}.{}", prefix, parts.join("."))
    }
}

/// Extract project name from pyproject.toml (simple parser for MVP).
fn extract_toml_project_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name ") {
            if let Some(rest) = rest.strip_prefix('=') {
                let name = rest.trim().trim_matches('"').trim_matches('\'');
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Map capture name to SymbolKind.
fn py_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.class" => Some(SymbolKind::Class),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn py_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.decorator" => Some(ReferenceKind::Decoration),
        "reference.usage" => Some(ReferenceKind::Usage),
        _ => None,
    }
}

/// Extract import info from capture.
fn py_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String, bool)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let is_relative = text.starts_with('.');
            Some((ImportKind::Import, text, String::new(), is_relative))
        }
        "import.name" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Import, String::new(), name, false))
        }
        "import.alias" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Import, String::new(), name, false))
        }
        "import.wildcard" => Some((
            ImportKind::FromImport,
            String::new(),
            "*".into(),
            false,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let adapter = PythonAdapter;
        assert_eq!(adapter.language(), Language::Python);
        assert!(adapter.extensions().contains(&"py"));
        assert!(!adapter.definition_query().is_empty());
        assert!(!adapter.reference_query().is_empty());
    }

    #[test]
    fn test_def_query_parses() {
        let adapter = PythonAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(query.is_ok(), "Python def query must compile");
    }

    #[test]
    fn test_ref_query_parses() {
        let adapter = PythonAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.reference_query());
        assert!(query.is_ok(), "Python ref query must compile");
    }

    #[test]
    fn test_py_definition_kind() {
        assert_eq!(
            py_definition_kind("definition.function"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            py_definition_kind("definition.class"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            py_definition_kind("definition.variable"),
            Some(SymbolKind::Variable)
        );
        assert_eq!(py_definition_kind("unknown"), None);
    }
}
