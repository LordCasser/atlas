//! Java LanguageAdapter.
//!
//! Provides query-driven extraction for Java source files.
//! Supports: class, interface, enum, method, field, constant, variable definitions;
//! method calls, field access, type references; import/include; scopes.

use crate::extraction::languages::{node_range, node_text, LanguageAdapter};
use crate::types::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Java LanguageAdapter.
pub struct JavaAdapter;

impl LanguageAdapter for JavaAdapter {
    fn language(&self) -> Language {
        Language::Java
    }

    fn extensions(&self) -> &[&str] {
        &["java"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../queries/java/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../queries/java/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../queries/java/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../queries/java/scopes.scm")
    }

    fn dataflow_query(&self) -> &str {
        include_str!("../queries/java/dataflow.scm")
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

        let kind = java_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);

        let qualified_name = qualified_name_from_node_java("", &name, node, source);
        let lang = self.language();
        let signature = java_extract_signature(capture_name, node, source);

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
        let kind = java_reference_kind(capture_name)?;
        let text = node_text(node, source)?;
        let name = text.clone();
        let range = node_range(node);

        // Find enclosing function for source_symbol
        let source_symbol = find_enclosing_method_id(node, source, file_id, self.language());

        let ref_id = ReferenceId::generate(
            &file_id,
            source_symbol.as_ref(),
            range.start_byte,
            range.end_byte,
            &text,
            kind,
        );

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
        let (kind, module, imported_name) = java_import_info(capture_name, node, source)?;
        let range = node_range(node);
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
            imported_name: imported_name.clone(),
            local_name: Some(imported_name),
            is_wildcard,
            is_relative: false, // Java imports are always absolute
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
            "scope.class" => ScopeKind::Class,
            "scope.interface" => ScopeKind::Interface,
            "scope.enum" => ScopeKind::Enum,
            "scope.method" => ScopeKind::Method,
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

    fn normalize_dataflow(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<RawEdge> {
        let kind_str = java_dataflow_kind(capture_name)?;
        let kind = EdgeKind::from_str(kind_str).unwrap_or(EdgeKind::Assigns);
        let text = node_text(node, source)?;

        let lang = self.language();
        let source_sym = find_enclosing_method_id(node, source, file_id, lang)?;

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

    fn detect_package(&self, _source: &str, file_path: &Path) -> Option<String> {
        // Java uses directory structure for packages
        // Look for pom.xml or build.gradle in parent directories
        let mut current = file_path.parent()?;
        loop {
            if current.join("pom.xml").exists() {
                if let Ok(content) = std::fs::read_to_string(current.join("pom.xml")) {
                    // Simple Maven groupId:artifactId extraction
                    if let Some(start) = content.find("<groupId>") {
                        if let Some(end) = content[start..].find("</groupId>") {
                            let gid = content[start + 9..start + end].trim();
                            if !gid.is_empty() {
                                return Some(gid.to_string());
                            }
                        }
                    }
                }
            }
            if current.join("build.gradle").exists() || current.join("build.gradle.kts").exists() {
                return Some(current.file_name()?.to_str()?.to_string());
            }
            current = current.parent()?;
        }
    }

    fn detect_frameworks(&self, source: &str) -> Vec<String> {
        let mut frameworks = Vec::new();
        if source.contains("org.springframework") {
            frameworks.push("spring".into());
        }
        if source.contains("android.") || source.contains("androidx.") {
            frameworks.push("android".into());
        }
        if source.contains("jakarta.") || source.contains("javax.") {
            frameworks.push("jakarta-ee".into());
        }
        frameworks
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a Java symbol from its parent hierarchy.
fn qualified_name_from_node_java(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
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

/// Walk up the tree to find the enclosing method/constructor and compute its SymbolId.
fn find_enclosing_method_id(
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    lang: Language,
) -> Option<SymbolId> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let parent_kind = parent.kind();
        match parent_kind {
            "method_declaration" | "constructor_declaration" => {
                let name_node = parent.child_by_field_name("name");
                let fn_name = name_node
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("anonymous");
                let qualified_name = qualified_name_from_node_java("", fn_name, parent, source);
                return Some(SymbolId::generate(
                    &file_id,
                    lang.as_str(),
                    &qualified_name,
                    SymbolKind::Method.as_str(),
                    None::<&str>,
                ));
            }
            "class_declaration" | "interface_declaration" => {
                // If we hit a class before a method, we're at class scope
                let name_node = parent.child_by_field_name("name");
                let class_name = name_node
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("anonymous");
                let qualified_name = qualified_name_from_node_java("", class_name, parent, source);
                return Some(SymbolId::generate(
                    &file_id,
                    lang.as_str(),
                    &qualified_name,
                    SymbolKind::Class.as_str(),
                    None::<&str>,
                ));
            }
            _ => {
                current = parent;
            }
        }
    }
    None
}

/// Map capture name to SymbolKind.
fn java_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.method" => Some(SymbolKind::Method),
        "definition.field" => Some(SymbolKind::Field),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn java_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn java_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let path = node_text(node, source)?;
            // Last segment is the imported name
            let name = path.rsplit('.').next().unwrap_or(&path).to_string();
            Some((ImportKind::Import, path, name))
        }
        "import.wildcard" => {
            let path = node_text(node, source)?;
            let module = path.trim_end_matches(".*").to_string();
            Some((ImportKind::FromImport, module, "*".to_string()))
        }
        _ => None,
    }
}

/// Map dataflow capture name to EdgeKind string.
fn java_dataflow_kind(capture_name: &str) -> Option<&'static str> {
    match capture_name {
        "dataflow.parameter" => Some("parameter"),
        "dataflow.return" => Some("returns"),
        "dataflow.assign" => Some("assigns"),
        "dataflow.field_write" => Some("field_write"),
        "dataflow.field_read" => Some("field_read"),
        _ => None,
    }
}

/// Extract method/constructor signature (formal parameters) from the AST.
///
/// The `node` is the identifier captured by `@definition.method` or
/// `@definition.constructor`. Its parent is `method_declaration` or
/// `constructor_declaration`, which has a `formal_parameters` child.
fn java_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.method" | "definition.constructor" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let adapter = JavaAdapter;
        assert_eq!(adapter.language(), Language::Java);
        assert!(adapter.extensions().contains(&"java"));
        assert!(!adapter.definition_query().is_empty());
    }

    #[test]
    fn test_def_query_parses() {
        let adapter = JavaAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(query.is_ok(), "definition query must compile: {:?}", query.err());
    }

    #[test]
    fn test_ref_query_parses() {
        let adapter = JavaAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.reference_query());
        assert!(query.is_ok(), "reference query must compile: {:?}", query.err());
    }

    #[test]
    fn test_import_query_parses() {
        let adapter = JavaAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.import_query());
        assert!(query.is_ok(), "import query must compile: {:?}", query.err());
    }

    #[test]
    fn test_scope_query_parses() {
        let adapter = JavaAdapter;
        let lang = adapter.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, adapter.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }
}
