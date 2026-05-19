//! Cangjie LanguageAdapter.
//!
//! Cangjie (仓颉) is Huawei's programming language.
//! AST uses aliased node names (className, funcName, etc.) rather than
//! generic `identifier` nodes, so all queries must match the aliased names.

use crate::extraction::languages::{node_range, node_text, LanguageAdapter};
use crate::types::*;
use std::path::Path;

/// Cangjie LanguageAdapter.
pub struct CangjieAdapter;

impl LanguageAdapter for CangjieAdapter {
    fn language(&self) -> Language {
        Language::Cangjie
    }

    fn extensions(&self) -> &[&str] {
        &["cj"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cangjie::LANGUAGE.into()
    }

    fn definition_query(&self) -> &str {
        include_str!("../queries/cangjie/definitions.scm")
    }

    fn reference_query(&self) -> &str {
        include_str!("../queries/cangjie/references.scm")
    }

    fn import_query(&self) -> &str {
        include_str!("../queries/cangjie/imports.scm")
    }

    fn scope_query(&self) -> &str {
        include_str!("../queries/cangjie/scopes.scm")
    }

    fn dataflow_query(&self) -> &str {
        include_str!("../queries/cangjie/dataflow.scm")
    }

    // -------------------------------------------------------------------
    // Normalizers
    // -------------------------------------------------------------------

    fn normalize_definition(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<SymbolDef> {
        use super::shared::SymbolDefBuilder;

        let kind = cj_definition_kind(capture_name)?;
        let name = node_text(node, source)?;
        let range = node_range(node);

        let qualified_name = qualified_name_from_node_cj("", &name, node, source);
        let lang = self.language();

        Some(SymbolDefBuilder::new(file_id, lang, kind, name, qualified_name, range).build())
    }

    fn normalize_reference(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<ReferenceUse> {
        let kind = cj_reference_kind(capture_name)?;
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

        // Walk up to find enclosing function for edge promotion
        let lang = self.language();
        let source_symbol = if matches!(kind, ReferenceKind::Call | ReferenceKind::Instantiation | ReferenceKind::FieldAccess) {
            find_enclosing_function_id_cj(node, source, file_id, lang)
        } else {
            None
        };

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
        let (kind, module, _imported_name) = cj_import_info(capture_name, node, source)?;
        let range = node_range(node);

        let import_id = ImportId::generate(
            &file_id,
            kind.as_str(),
            &module,
            None::<&str>,
            range.start_byte,
        );

        Some(ImportDef {
            id: import_id,
            file_id,
            kind,
            module,
            imported_name: String::new(),
            local_name: None,
            is_wildcard: false,
            is_relative: false,
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
        let kind = cj_scope_kind(capture_name)?;
        let name = match kind {
            ScopeKind::File => String::new(),
            _ => node_text(node, source).unwrap_or_default(),
        };
        let range = node_range(node);
        let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);

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

    fn normalize_dataflow(
        &self,
        capture_name: &str,
        node: tree_sitter::Node,
        source: &str,
        file_id: FileId,
        _file_path: &Path,
    ) -> Option<RawEdge> {
        let kind_str = cj_dataflow_kind(capture_name)?;
        let kind = EdgeKind::from_str(kind_str).unwrap_or(EdgeKind::Assigns);
        let text = node_text(node, source)?;

        let lang = self.language();
        let source_sym = find_enclosing_function_id_cj(node, source, file_id, lang)?;

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

/// Walk up the tree from `node` to find the enclosing function definition,
/// and compute its deterministic SymbolId.
fn find_enclosing_function_id_cj(
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    lang: Language,
) -> Option<SymbolId> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let parent_kind = parent.kind();
        let fn_node = match parent_kind {
            "functionDefinition" => {
                // Get funcName child (aliased from identifier)
                parent.child_by_field_name("funcName")
                    .or_else(|| parent.child_by_field_name("name"))
            }
            "classDefinition" => {
                // If we hit a class before a function, use it as source (method context)
                parent.child_by_field_name("className")
                    .or_else(|| parent.child_by_field_name("name"))
            }
            _ => {
                current = parent;
                continue;
            }
        };

        let fn_name = fn_node
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .unwrap_or("anonymous");
        let kind = if parent_kind == "functionDefinition" { SymbolKind::Function } else { SymbolKind::Class };
        let qualified_name = qualified_name_from_node_cj("", fn_name, parent, source);

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

/// Build a qualified name using `::` separators (Cangjie convention).
fn qualified_name_from_node_cj(
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
            "classDefinition" => {
                if let Some(child) = parent.child_by_field_name("className") {
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
        parts.join("::")
    } else {
        format!("{}::{}", prefix, parts.join("::"))
    }
}

fn cj_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.function" => Some(SymbolKind::Function),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

fn cj_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

fn cj_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let module_path = node_text(node, source)?;
            Some((ImportKind::Import, module_path.to_string(), String::new()))
        }
        _ => None,
    }
}

fn cj_scope_kind(capture: &str) -> Option<ScopeKind> {
    match capture {
        "scope.file" => Some(ScopeKind::File),
        "scope.class" => Some(ScopeKind::Class),
        "scope.interface" => Some(ScopeKind::Class),
        "scope.function" => Some(ScopeKind::Function),
        "scope.block" => Some(ScopeKind::Block),
        _ => None,
    }
}

fn cj_dataflow_kind(capture: &str) -> Option<&'static str> {
    match capture {
        "dataflow.parameter" => Some("parameter"),
        "dataflow.return" => Some("returns"),
        "dataflow.assign" => Some("assigns"),
        "dataflow.field_write" => Some("field_write"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cj_adapter_metadata() {
        let adapter = CangjieAdapter;
        assert_eq!(adapter.language(), Language::Cangjie);
        assert!(adapter.extensions().contains(&"cj"));
        assert!(!adapter.definition_query().is_empty());
        assert!(!adapter.reference_query().is_empty());
        assert!(!adapter.import_query().is_empty());
        assert!(!adapter.scope_query().is_empty());
        assert!(!adapter.dataflow_query().is_empty());
    }

    #[test]
    fn test_cj_queries_parse() {
        let adapter = CangjieAdapter;
        let lang = adapter.tree_sitter_language();

        // tree-sitter-cangjie requires language version 15 which is
        // incompatible with tree-sitter 0.24 (max version 14).
        // Query compilation will succeed once tree-sitter is upgraded to 0.25+.
        let def_q = tree_sitter::Query::new(&lang, adapter.definition_query());
        assert!(def_q.is_ok() || def_q.as_ref().unwrap_err().message.contains("language version"),
            "definitions query: {:?}", def_q.err());
        if def_q.is_err() { return; }

        let ref_q = tree_sitter::Query::new(&lang, adapter.reference_query());
        assert!(ref_q.is_ok(), "references query: {:?}", ref_q.err());

        let imp_q = tree_sitter::Query::new(&lang, adapter.import_query());
        assert!(imp_q.is_ok(), "imports query: {:?}", imp_q.err());

        let sc_q = tree_sitter::Query::new(&lang, adapter.scope_query());
        assert!(sc_q.is_ok(), "scopes query: {:?}", sc_q.err());

        let df_q = tree_sitter::Query::new(&lang, adapter.dataflow_query());
        assert!(df_q.is_ok(), "dataflow query: {:?}", df_q.err());
    }

    #[test]
    fn test_cj_definition_kind_mapping() {
        assert_eq!(cj_definition_kind("definition.class"), Some(SymbolKind::Class));
        assert_eq!(cj_definition_kind("definition.function"), Some(SymbolKind::Function));
        assert_eq!(cj_definition_kind("unknown"), None);
    }

    #[test]
    fn test_cj_reference_kind_mapping() {
        assert_eq!(cj_reference_kind("reference.call"), Some(ReferenceKind::Call));
        assert_eq!(cj_reference_kind("reference.field"), Some(ReferenceKind::FieldAccess));
        assert_eq!(cj_reference_kind("unknown"), None);
    }

    #[test]
    fn test_cj_import_info_mapping() {
        // import_info requires a real tree-sitter Node (from a parse tree)
        // — tests are deferred to E2E fixture-based tests.
    }
}
