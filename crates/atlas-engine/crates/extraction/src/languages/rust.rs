//! Rust frontend spec (slot-based).
//!
//! Provides query-driven extraction for Rust source files.
//! Supports: function, method, struct, enum, enum_variant, trait, module, variable,
//! constant, type_alias, macro, field definitions; function calls, macro invocations,
//! field access, type references; use/extern_crate imports; scopes.

use crate::languages::{node_range, node_text};

use crate::dataflow_builder::NodePosKey;
use crate::extraction_ctx::ExtractionCtx;
use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::{
    SymbolDefBuilder, make_binding_def, make_df_assign_target, make_df_assign_value,
    make_df_call_arg, make_df_parameter, make_df_receiver_or_literal, make_df_return_value,
    make_reference_use, make_scope_def_auto_name,
};
use std::collections::HashMap;
use types::bindings::BindingDef;
use types::capability::FeatureSupport;
use types::dataflow::{DataFlowEdge, DataNode};
use types::enums::{DataFlowKind, DataNodeKind};
use types::ids::{DataFlowEdgeId, DataNodeId};
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Rust frontend spec.
pub(crate) struct RustAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_rust_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = rust_definition_kind(capture_name, node)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_rust("", &name, node, source);
    let signature = rust_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Rust, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_rust_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = rust_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_rust_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = rust_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let is_relative = module.starts_with("crate::") || module.starts_with("super::");

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
        is_wildcard: false,
        is_relative,
        range,
        alias: None,
    })
}

fn normalize_rust_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.module" => ScopeKind::Module,
        "scope.function" => ScopeKind::Function,
        "scope.class" => ScopeKind::Class, // struct/enum
        "scope.trait" => ScopeKind::Trait,
        "scope.block" => ScopeKind::Block,
        "scope.conditional" => ScopeKind::Conditional,
        "scope.loop" => ScopeKind::Loop,
        _ => return None,
    };
    let range = node_range(node);

    Some(make_scope_def_auto_name(file_id, kind, range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for RustAdapter {
    fn language(&self) -> Language {
        Language::Rust
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for RustAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/rust/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/rust/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_rust_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for RustAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/rust/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_rust_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for RustAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/rust/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_rust_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for RustAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/rust/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_rust_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for RustAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/rust/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_rust_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for RustAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/rust/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["AST-driven local dataflow with language-specific gaps"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_rust_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn build_language_edges(
        &self,
        ctx: &ExtractionCtx<'_>,
        pos_map: &HashMap<NodePosKey, DataNodeId>,
        _nodes: &[DataNode],
        _bindings: &[BindingDef],
        _scopes: &[ScopeDef],
        edges: &mut Vec<DataFlowEdge>,
    ) -> anyhow::Result<()> {
        walk_rust_assign_edges(ctx.root, pos_map, edges);
        Ok(())
    }
}

/// Walk the AST for Rust-specific let_declaration patterns.
fn walk_rust_assign_edges(
    node: tree_sitter::Node,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    if node.kind() == "let_declaration" {
        if let (Some(pattern_node), Some(value_node)) = (
            node.child_by_field_name("pattern"),
            node.child_by_field_name("value"),
        ) {
            let value_key = NodePosKey {
                start_byte: value_node.start_byte() as u32,
                end_byte: value_node.end_byte() as u32,
                kind: DataNodeKind::Expr,
            };
            if let Some(&source_id) = pos_map.get(&value_key) {
                if pattern_node.kind() == "identifier" {
                    let name_key = NodePosKey {
                        start_byte: pattern_node.start_byte() as u32,
                        end_byte: pattern_node.end_byte() as u32,
                        kind: DataNodeKind::Local,
                    };
                    if let Some(&target_id) = pos_map.get(&name_key) {
                        let eid = DataFlowEdgeId::generate(
                            &source_id,
                            &target_id,
                            DataFlowKind::Assign.as_str(),
                        );
                        edges.push(DataFlowEdge::new(
                            eid,
                            source_id,
                            target_id,
                            DataFlowKind::Assign,
                            node_range(pattern_node),
                            0.90,
                        ));
                    }
                } else if matches!(
                    pattern_node.kind(),
                    "tuple_pattern" | "tuple_struct_pattern"
                ) {
                    for i in 0..pattern_node.child_count() {
                        if let Some(child) = pattern_node.child(i as u32) {
                            if child.is_named() && child.kind() == "identifier" {
                                let child_key = NodePosKey {
                                    start_byte: child.start_byte() as u32,
                                    end_byte: child.end_byte() as u32,
                                    kind: DataNodeKind::Local,
                                };
                                if let Some(&target_id) = pos_map.get(&child_key) {
                                    let eid = DataFlowEdgeId::generate(
                                        &source_id,
                                        &target_id,
                                        DataFlowKind::Assign.as_str(),
                                    );
                                    edges.push(DataFlowEdge::new(
                                        eid,
                                        source_id,
                                        target_id,
                                        DataFlowKind::Assign,
                                        node_range(child),
                                        0.90,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_rust_assign_edges(child, pos_map, edges);
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn rust_frontend() -> LanguageFrontend {
    let lang = Language::Rust;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(RustAdapter),
        symbols: Box::new(RustAdapter),
        references: Box::new(RustAdapter),
        imports: Box::new(RustAdapter),
        scopes: Box::new(RustAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(RustAdapter),
        dataflow: Box::new(RustAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a Rust symbol from parent module/type hierarchy.
fn qualified_name_from_node_rust(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "struct_item" | "enum_item" | "trait_item" | "impl_item" => {
                if let Some(type_name) = parent.child_by_field_name("name") {
                    if let Ok(type_str) = type_name.utf8_text(source.as_bytes()) {
                        parts.push(type_str.to_string());
                    }
                } else if parent.kind() == "impl_item" {
                    // impl block for a type — extract the type name from impl type
                    if let Some(impl_type) = parent.child_by_field_name("type") {
                        if let Ok(type_str) = impl_type.utf8_text(source.as_bytes()) {
                            parts.push(type_str.to_string());
                        }
                    }
                }
            }
            "mod_item" => {
                if let Some(mod_name) = parent.child_by_field_name("name") {
                    if let Ok(mod_str) = mod_name.utf8_text(source.as_bytes()) {
                        parts.push(mod_str.to_string());
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

/// Map capture name to SymbolKind.
/// For `definition.function`, checks if the function is inside an impl block
/// (making it a method).
fn rust_definition_kind(capture: &str, node: tree_sitter::Node) -> Option<SymbolKind> {
    match capture {
        "definition.function" => {
            // Check if this function is inside an impl_item or trait_item — if so, it's a method
            if is_inside_impl(node) {
                Some(SymbolKind::Method)
            } else {
                Some(SymbolKind::Function)
            }
        }
        "definition.class" => Some(SymbolKind::Struct), // struct in Rust
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.enum_member" => Some(SymbolKind::EnumMember),
        "definition.trait" => Some(SymbolKind::Trait),
        "definition.module" => Some(SymbolKind::Module),
        "definition.variable" => Some(SymbolKind::Variable),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.type_alias" => Some(SymbolKind::TypeAlias),
        "definition.macro" => Some(SymbolKind::Macro),
        "definition.field" => Some(SymbolKind::Field),
        _ => None,
    }
}

/// Check if a node is inside an impl_item.
fn is_inside_impl(node: tree_sitter::Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            return true;
        }
        // Stop at module/file boundary
        if matches!(parent.kind(), "source_file" | "mod_item") {
            return false;
        }
        current = parent.parent();
    }
    false
}

/// Map capture name to ReferenceKind.
fn rust_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn rust_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            // Determine kind from parent context
            let parent = node.parent()?;
            let kind = if parent.kind() == "extern_crate_declaration" {
                ImportKind::Import
            } else {
                ImportKind::Use
            };
            // Last segment is the imported name
            let name = text.rsplit("::").next().unwrap_or(&text).to_string();
            Some((kind, text, name))
        }
        _ => None,
    }
}

/// Extract function/method signature from the AST.
fn rust_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            let param_text = node_text(params, source)?;
            // Include return type if present
            if let Some(ret) = parent.child_by_field_name("return_type") {
                let ret_text = node_text(ret, source)?;
                Some(format!("{param_text} {ret_text}"))
            } else {
                Some(param_text)
            }
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn rust_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn normalize_rust_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = rust_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_rust_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => make_df_parameter(file_id, node, source, range),
        "df.assign_target" => make_df_assign_target(file_id, node, source, range),
        "df.assign_value" => {
            make_df_assign_value(file_id, node, source, range, &["call_expression"])
        }
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.tail_return" => {
            // Block tail expression (implicit return). Filter out non-expression
            // nodes like let_declaration that may also match as the last child.
            let kind = node.kind();
            if matches!(kind, "let_declaration" | "expression_statement") {
                return (None, None);
            }
            // Only create Return nodes for direct function/closure body tails.
            // Inner block tails (if/match arm, block expression like
            // `let y = { x }`) are expression values, not function returns.
            let is_fn_tail = node
                .parent()
                .filter(|p| p.kind() == "block")
                .and_then(|b| b.parent())
                .is_some_and(|p| matches!(p.kind(), "function_item" | "closure_expression"));
            let text = node_text(node, source).unwrap_or_default();
            let data_kind = if is_fn_tail {
                DataNodeKind::Return
            } else {
                DataNodeKind::Expr
            };
            let kind_tag = if is_fn_tail { "return" } else { "expr" };
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                kind_tag,
                Some(&text),
                None,
                range.start_byte,
            );
            (
                Some(DataNode {
                    id: node_id,
                    file_id,
                    function_id: None,
                    kind: data_kind,
                    binding_id: None,
                    callsite_id: None,
                    name: Some(text),
                    access_path: None,
                    arg_index: None,
                    range,
                }),
                None,
            )
        }
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = name.clone();
                let callsite_id = crate::languages::shared::find_call_expression(
                    node,
                    &["call_expression"],
                )
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "call_target",
                    Some(&name),
                    Some(&access_path),
                    range.start_byte,
                );
                (
                    Some(DataNode::call_target(
                        node_id,
                        file_id,
                        None,
                        callsite_id,
                        &name,
                        &access_path,
                        range,
                    )),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.call_arg" => make_df_call_arg(file_id, node, source, range, &["call_expression"]),
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "field_expression")
                    .and_then(|p| node_text(p, source))
                    .unwrap_or_else(|| name.clone());
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "field",
                    Some(&name),
                    Some(&access_path),
                    range.start_byte,
                );
                (
                    Some(DataNode::field(
                        node_id,
                        file_id,
                        None,
                        &name,
                        &access_path,
                        range,
                    )),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.receiver" | "df.literal" => {
            make_df_receiver_or_literal(file_id, capture_name, node, source, range)
        }
        "df.identifier_use" => {
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &[
                    "use_declaration",
                    "mod_item",
                    "struct_item",
                    "enum_item",
                    "trait_item",
                    "impl_item",
                ],
            ) {
                return (None, None);
            }
            let text = node_text(node, source).unwrap_or_default();
            if text.is_empty() {
                return (None, None);
            }
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "identifier_use",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: DataNodeKind::VariableUse,
                binding_id: None,
                callsite_id: None,
                name: Some(text.clone()),
                access_path: Some(text),
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let spec = RustAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = RustAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(
            query.is_ok(),
            "definition query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_ref_query_parses() {
        let spec = RustAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(
            query.is_ok(),
            "reference query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_import_query_parses() {
        let spec = RustAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.import_query());
        assert!(
            query.is_ok(),
            "import query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_scope_query_parses() {
        let spec = RustAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = super::rust_frontend();
        let lang = spec.parser.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_smoke() {
        let frontend = rust_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = r#"fn foo(x: i32) -> i32 {
    let y = x;
    let result = bar(y, 42);
    result
}
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.ext");
        let ctx = NormalizeCtx {
            language: frontend.parser.language(),
            file_id,
            file_path: std::path::Path::new("test.ext"),
            source,
        };

        let mut nodes: Vec<DataNode> = Vec::new();
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            let (dn, _de) = frontend.dataflow.normalize(
                ctx,
                Capture {
                    name,
                    node: cap.node,
                },
            );
            if let Some(dn) = dn {
                nodes.push(dn);
            }
        }

        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::Parameter),
            "param"
        );
        assert!(nodes.iter().any(|n| n.kind == DataNodeKind::Local), "local");
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::VariableUse),
            "varuse"
        );
    }
}
