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
        let exported = is_exported_in_tree(node);

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
            exported,
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

        // Populate source_symbol by walking up to the enclosing function/class.
        // This enables edge promotion (Calls→Instantiates/Implements) in the resolver.
        let source_symbol = find_enclosing_function_id(node, source, file_id, self.language());

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
            "scope.method" => ScopeKind::Method,
            "scope.class" => ScopeKind::Class,
            "scope.interface" => ScopeKind::Interface,
            "scope.enum" => ScopeKind::Enum,
            "scope.namespace" => ScopeKind::Namespace,
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

    fn dataflow_query(&self) -> &str {
        include_str!("../queries/typescript/dataflow.scm")
    }

    fn normalize_dataflow(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<RawEdge> {
        let kind_str = ts_dataflow_kind(capture_name)?;
        let kind = EdgeKind::from_str(kind_str).unwrap_or(EdgeKind::Assigns);
        let text = node_text(node, source)?;

        // Find the enclosing function/method to use as the dataflow source.
        // Skip dataflow edges that are not inside a function — the source
        // SymbolId must match an existing symbol in the database.
        let lang = self.language();
        let source_sym = find_enclosing_function_id(node, source, file_id, lang)?;

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
        Some(RawEdge {
            id: edge_id,
            source: source_sym,
            target,
            kind,
            confidence: Confidence::certain(),
            provenance: Provenance::TreeSitter,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map dataflow capture name to EdgeKind string.
fn ts_dataflow_kind(capture_name: &str) -> Option<&'static str> {
    match capture_name {
        "dataflow.parameter" => Some("parameter"),
        "dataflow.return" => Some("returns"),
        "dataflow.assign" => Some("assigns"),
        "dataflow.field_write" => Some("field_write"),
        "dataflow.field_read" => Some("field_read"),
        _ => None,
    }
}

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

/// Check whether a TS/JS node is inside an `export` statement.
fn is_exported_in_tree(node: tree_sitter::Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let kind = parent.kind();
        if kind == "export_statement" || kind.contains("export") {
            return true;
        }
        // Stop at the top-level declaration container
        if kind == "program" || kind == "statement_block" {
            break;
        }
        current = parent;
    }
    false
}

/// Walk up the tree from `node` to find the enclosing function/method declaration,
/// and compute its deterministic SymbolId using the same logic as `normalize_definition`.
fn find_enclosing_function_id(
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    lang: Language,
) -> Option<SymbolId> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let parent_kind = parent.kind();
        // Match function-like declarations
        let (name_node, kind) = match parent_kind {
            "function_declaration" => {
                (parent.child_by_field_name("name"), SymbolKind::Function)
            }
            "method_definition" => {
                (parent.child_by_field_name("name"), SymbolKind::Method)
            }
            "arrow_function" => {
                // Arrow functions don't have a "name" field — walk up to find the
                // enclosing variable declarator for the name.
                let mut n = parent;
                let arrow_name = loop {
                    if let Some(p) = n.parent() {
                        match p.kind() {
                            "variable_declarator" => {
                                break p.child_by_field_name("name");
                            }
                            "assignment_expression" => {
                                break p.child_by_field_name("left")
                                    .and_then(|left| left.child_by_field_name("property"));
                            }
                            _ => {
                                n = p;
                                continue;
                            }
                        }
                    } else {
                        break None;
                    }
                };
                (arrow_name, SymbolKind::Function)
            }
            _ => {
                current = parent;
                continue;
            }
        };

        let fn_name = name_node
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("anonymous");
        let qualified_name = qualified_name_from_node("", fn_name, parent, source);

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
