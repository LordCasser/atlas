//! Python LanguageAdapter implementation.
//!
//! Uses tree-sitter-python grammar and embedded query files.

use crate::extraction::languages::{node_range, node_text, LanguageAdapter};
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
        use super::shared::SymbolDefBuilder;

        let kind = py_definition_kind(capture_name, node)?;
        let name = node_text(node, source)?;
        let range = node_range(node);

        let qualified_name = qualified_name_from_node_py("", &name, node, source);
        let lang = self.language();
        let exported = is_exported_in_tree_py(node, &name);
        let signature = py_extract_signature(capture_name, node, source);

        Some(
            SymbolDefBuilder::new(file_id, lang, kind, name, qualified_name, range)
                .signature(signature)
                .exported(exported)
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

        // Populate source_symbol by walking up to the enclosing function.
        let source_symbol = find_enclosing_function_id_py(node, source, file_id, self.language());

        Some(ReferenceUse {
            id: ref_id,
            file_id,
            source_symbol,
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
            "scope.conditional" => ScopeKind::Conditional,
            "scope.loop" => ScopeKind::Loop,
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

    fn dataflow_query(&self) -> &str {
        include_str!("../queries/python/dataflow.scm")
    }

    fn normalize_dataflow(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<RawEdge> {
        let kind_str = py_dataflow_kind(capture_name)?;
        let kind = EdgeKind::from_str(kind_str).unwrap_or(EdgeKind::Assigns);
        let text = node_text(node, source)?;

        // Find the enclosing function to use as the dataflow source.
        // Skip dataflow edges that are not inside a function.
        let lang = self.language();
        let source_sym = find_enclosing_function_id_py(node, source, file_id, lang)?;

        let target = SymbolId::generate(
            &file_id,
            "dataflow",
            &text,
            kind_str,
            None::<&str>,
        );
        let edge_id = EdgeId::generate(
            &source_sym,
            &target,
            kind_str,
            None::<&ReferenceId>,
            Provenance::TreeSitter.as_str(),
        );
        Some(RawEdge::new(
            edge_id,
            source_sym,
            target,
            kind,
            Confidence::certain(),
            Provenance::TreeSitter,
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map dataflow capture name to EdgeKind string.
fn py_dataflow_kind(capture_name: &str) -> Option<&'static str> {
    match capture_name {
        "dataflow.parameter" => Some("parameter"),
        "dataflow.return" => Some("returns"),
        "dataflow.assign" => Some("assigns"),
        "dataflow.field_write" => Some("field_write"),
        "dataflow.field_read" => Some("field_read"),
        _ => None,
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
                        // Skip if the class name equals the current segment's name
                        // to avoid double-counting when starting from the name child.
                        if class_name != name {
                            parts.push(class_name.to_string());
                        }
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

/// Check if a Python definition is exported (module-level, no leading underscore).
fn is_exported_in_tree_py(node: tree_sitter::Node, name: &str) -> bool {
    if name.starts_with('_') {
        return false;
    }
    // Walk up to check if we're at the module (file) scope
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "module" => return true,      // Top-level definition
            "class_definition" => return true, // Class member (public by convention)
            "function_definition" | "lambda" => return false, // Nested in function → not exported
            _ => {}
        }
        current = parent;
    }
    false
}

/// Walk up the tree from `node` to find the enclosing function definition,
/// and compute its deterministic SymbolId.
fn find_enclosing_function_id_py(
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    lang: Language,
) -> Option<SymbolId> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let parent_kind = parent.kind();
        let (name_node, kind) = match parent_kind {
            "function_definition" => {
                (parent.child_by_field_name("name"), SymbolKind::Function)
            }
            "lambda" => {
                // Lambdas are anonymous — skip and continue walking up to
                // the enclosing named function/class. This avoids creating
                // SymbolIds that don't exist in the symbols table (which
                // would cause FOREIGN KEY violations on edges/callsites).
                current = parent;
                continue;
            }
            "class_definition" => {
                // If we hit a class before a function, we're at class scope (method)
                (parent.child_by_field_name("name"), SymbolKind::Class)
            }
            _ => {
                current = parent;
                continue;
            }
        };

        let fn_name = name_node
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("anonymous");
        let qualified_name = qualified_name_from_node_py("", fn_name, parent, source);

        return Some(SymbolId::generate(
            &file_id,
            lang.as_str(),
            &qualified_name,
            kind.as_str(),
            None::<&str>,
        ));
    }
    None
}

/// Map capture name to SymbolKind.
fn py_definition_kind(capture: &str, node: tree_sitter::Node) -> Option<SymbolKind> {
    match capture {
        "definition.function" => {
            // A function_definition inside a class_definition is a method.
            // `node` is the identifier; walk up from its parent (function_definition).
            let mut cursor = node.parent(); // function_definition
            while let Some(p) = cursor {
                match p.kind() {
                    "class_definition" => return Some(SymbolKind::Method),
                    "module" => break,
                    _ => cursor = p.parent(),
                }
            }
            Some(SymbolKind::Function)
        }
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

/// Extract function/method signature (parameter list) from the AST.
///
/// The `node` is the identifier captured by `@definition.function` or `@definition.class`.
/// For functions/methods, we walk to the parent `function_definition` and extract its
/// `parameters` child. For classes, we look for `__init__` parameters.
fn py_extract_signature(capture_name: &str, node: tree_sitter::Node, source: &str) -> Option<String> {
    match capture_name {
        "definition.function" => {
            // node is the identifier; parent is function_definition
            let func_def = node.parent()?;
            if func_def.kind() != "function_definition" {
                return None;
            }
            let params = func_def.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
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
            let module = extract_module_from_import_ancestor(node, source);
            Some((ImportKind::Import, module, name, false))
        }
        "import.alias" => {
            let name = node_text(node, source)?;
            let module = extract_module_from_import_ancestor(node, source);
            Some((ImportKind::Import, module, name, false))
        }
        "import.wildcard" => {
            let module = extract_module_from_import_ancestor(node, source);
            Some((
                ImportKind::FromImport,
                module,
                "*".into(),
                false,
            ))
        }
        _ => None,
    }
}

/// Walk up from a node inside an import_statement/import_from_statement
/// to find the module name (either from the `name` field or dotted_name).
fn extract_module_from_import_ancestor(node: tree_sitter::Node, source: &str) -> String {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "import_statement" => {
                // `import foo` — module is the name/dotted_name field
                if let Some(name_child) = parent.child_by_field_name("name") {
                    if let Some(m) = node_text(name_child, source) {
                        return m;
                    }
                }
                break;
            }
            "import_from_statement" => {
                // `from foo import bar` — module is the module_name field
                if let Some(module_name) = parent.child_by_field_name("module_name") {
                    if let Some(m) = node_text(module_name, source) {
                        return m;
                    }
                }
                break;
            }
            _ => {}
        }
        current = parent;
    }
    String::new()
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
    fn test_py_definition_kind_basic() {
        // We can't easily construct tree-sitter Nodes in unit tests,
        // so test the capture-name mapping only (without AST parent check).
        // The Method detection via parent walk is tested implicitly by the
        // integration test pipeline. Here we verify the fallback behavior:
        // when node has no class_definition parent, "definition.function" → Function.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();

        // Top-level function → Function
        let tree = parser.parse("def foo(): pass", None).unwrap();
        let root = tree.root_node();
        let func_node = root.child(0).unwrap().child_by_field_name("name").unwrap();
        assert_eq!(
            py_definition_kind("definition.function", func_node),
            Some(SymbolKind::Function)
        );

        // Method (function inside class) → Method
        let tree = parser.parse("class Foo:\n    def bar(self): pass", None).unwrap();
        let root = tree.root_node();
        // class → body → block → function_definition → name
        let class_node = root.child(0).unwrap();
        let body = class_node.child_by_field_name("body").unwrap();
        let func_def = body.child(0).unwrap();
        let method_name = func_def.child_by_field_name("name").unwrap();
        assert_eq!(
            py_definition_kind("definition.function", method_name),
            Some(SymbolKind::Method)
        );

        // Class → Class
        let class_name = class_node.child_by_field_name("name").unwrap();
        assert_eq!(
            py_definition_kind("definition.class", class_name),
            Some(SymbolKind::Class)
        );

        // Unknown → None
        assert_eq!(py_definition_kind("unknown", func_node), None);
    }
}
