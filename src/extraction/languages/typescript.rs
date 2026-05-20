//! TypeScript / JavaScript LanguageAdapter implementation.
//!
//! Uses tree-sitter-typescript grammar and embedded query files.
//! JavaScript is treated as a subset of TypeScript for extraction purposes.

use crate::extraction::languages::{node_range, node_text, LanguageAdapter};

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
        use super::shared::SymbolDefBuilder;

        let kind = ts_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);

        let qualified_name = qualified_name_from_node("", &name, node, source);
        let lang = self.language();
        let exported = is_exported_in_tree(node);

        Some(
            SymbolDefBuilder::new(file_id, lang, kind, name, qualified_name, range)
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

    fn lexical_query(&self) -> &str {
        include_str!("../queries/typescript/lexical.scm")
    }

    fn dataflow_builder_query(&self) -> &str {
        include_str!("../queries/typescript/dataflow_builder.scm")
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

    fn normalize_lexical(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<BindingDef> {
        let kind = ts_binding_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);
        // Use a zero-based scope_id placeholder; scope_id is resolved post-extraction
        // when we know the parent scope relationships.
        let scope_id = crate::types::ids::ScopeId::generate(
            &file_id,
            None::<&crate::types::ids::ScopeId>,
            kind.as_str(),
            range.start_byte,
        );
        let id = crate::types::ids::BindingId::generate(
            &file_id,
            &scope_id,
            kind.as_str(),
            &name,
            range.start_byte,
        );
        Some(BindingDef {
            id,
            file_id,
            function_id: None, // not resolved here; filled by post-extraction
            scope_id,
            kind,
            name,
            symbol_id: None,
            range,
        })
    }

    fn normalize_dataflow_builder(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        use crate::types::ids::DataNodeId;

        let range = node_range(node);

        match capture_name {
            "df.assign_target" => {
                node_text(node, source).map(|name| {
                    let node_id = DataNodeId::generate(
                        &file_id, None::<&crate::types::ids::SymbolId>,
                        "local", Some(&name), Some(&name), range.start_byte,
                    );
                    // FK fields are None at extraction — resolved post-extraction
                    let dn = DataNode::local(node_id, file_id, None, None, &name, range);
                    (Some(dn), None)
                }).unwrap_or((None, None))
            }
            "df.assign_value" => {
                let text = node_text(node, source).unwrap_or_default();
                let node_id = DataNodeId::generate(
                    &file_id, None::<&crate::types::ids::SymbolId>,
                    "expr", Some(&text), None, range.start_byte,
                );
                let dn = DataNode {
                    id: node_id, file_id, function_id: None,
                    kind: crate::types::enums::DataNodeKind::Expr, binding_id: None,
                    callsite_id: None, name: Some(text),
                    access_path: None, range,
                };
                (Some(dn), None)
            }
            "df.return_value" => {
                let node_id = DataNodeId::generate(
                    &file_id, None::<&crate::types::ids::SymbolId>,
                    "return", None, None, range.start_byte,
                );
                let dn = DataNode::return_(node_id, file_id, None, range);
                (Some(dn), None)
            }
            "df.call_arg" => {
                let text = node_text(node, source).unwrap_or_default();
                let node_id = DataNodeId::generate(
                    &file_id, None::<&crate::types::ids::SymbolId>,
                    "call_arg", Some(&text), None, range.start_byte,
                );
                let dn = DataNode::call_arg(node_id, file_id, None, None, Some(&text), range);
                (Some(dn), None)
            }
            "df.field_name" => {
                node_text(node, source).map(|name| {
                    let node_id = DataNodeId::generate(
                        &file_id, None::<&crate::types::ids::SymbolId>,
                        "field", Some(&name), Some(&name), range.start_byte,
                    );
                    let dn = DataNode::field(node_id, file_id, None, &name, &name, range);
                    (Some(dn), None)
                }).unwrap_or((None, None))
            }
            "df.literal" | "df.await_value" | "df.receiver" => {
                let text = node_text(node, source).unwrap_or_default();
                let node_id = DataNodeId::generate(
                    &file_id, None::<&crate::types::ids::SymbolId>,
                    "literal", Some(&text), None, range.start_byte,
                );
                let dn = DataNode {
                    id: node_id, file_id, function_id: None,
                    kind: crate::types::enums::DataNodeKind::Literal, binding_id: None,
                    callsite_id: None, name: Some(text),
                    access_path: None, range,
                };
                (Some(dn), None)
            }
            _ => (None, None),
        }
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

/// Map lexical capture name to BindingKind.
fn ts_binding_kind(capture_name: &str) -> Option<crate::types::enums::BindingKind> {
    use crate::types::enums::BindingKind;
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.import_alias" => Some(BindingKind::ImportAlias),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        "lexical.field" => Some(BindingKind::Field),
        _ => None,
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
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

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
            // Walk up to the enclosing import_statement to find the module source
            let module = extract_module_from_ancestor(node, source);
            Some((ImportKind::Import, module, name))
        }
        "import.namespace" => {
            let name = node_text(node, source)?;
            let module = extract_module_from_ancestor(node, source);
            Some((ImportKind::Import, module, name))
        }
        _ => None,
    }
}

/// Walk up from a node inside an import_statement to find the `source` field
/// (the module path string).
fn extract_module_from_ancestor(node: tree_sitter::Node, source: &str) -> String {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "import_statement" {
            if let Some(source_child) = parent.child_by_field_name("source") {
                if let Some(module_path) = node_text(source_child, source) {
                    return module_path.trim_matches(|c| c == '"' || c == '\'').to_string();
                }
            }
            break;
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
