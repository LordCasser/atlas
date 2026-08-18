//! C# frontend spec (slot-based).
//!
//! Provides query-driven extraction for C# source files.
//! Supports: class, struct, interface, enum, enum_member, method, constructor,
//! property, field, variable, namespace, delegate definitions;
//! method calls, field access, type references, instantiation; using directives;
//! scopes.

use crate::languages::{node_range, node_text};

use std::collections::HashMap;

use crate::dataflow_builder::NodePosKey;
use crate::extraction_ctx::ExtractionCtx;
use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::{
    SymbolDefBuilder, compact_signature, make_binding_def, make_df_assign_field_target,
    make_df_assign_target, make_df_assign_value, make_df_call_arg, make_df_parameter,
    make_df_receiver_or_literal, make_df_return_value, make_reference_use,
    make_scope_def_auto_name,
};
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// C# frontend spec.
pub(crate) struct CSharpAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_csharp_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = csharp_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_csharp("", &name, node, source);
    let signature = csharp_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::CSharp, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_csharp_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = csharp_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_csharp_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = csharp_import_info(capture_name, node, source)?;
    let range = node_range(node);

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
        is_relative: false,
        range,
        alias: None,
    })
}

fn normalize_csharp_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.namespace" => ScopeKind::Namespace,
        "scope.class" => ScopeKind::Class,
        "scope.interface" => ScopeKind::Interface,
        "scope.method" => ScopeKind::Method,
        "scope.block" => ScopeKind::Block,
        "scope.conditional" => ScopeKind::Conditional,
        "scope.loop" => ScopeKind::Loop,
        _ => return None,
    };
    let range = node_range(node);

    Some(make_scope_def_auto_name(file_id, kind, range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for CSharpAdapter {
    fn language(&self) -> Language {
        Language::CSharp
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for CSharpAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/csharp/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/csharp/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_csharp_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for CSharpAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/csharp/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_csharp_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for CSharpAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/csharp/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_csharp_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for CSharpAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/csharp/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_csharp_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for CSharpAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/csharp/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.72,
            vec![
                "scope-chain-aware parameter/local/catch/pattern binding; switch pattern captures are arm-scoped, while definite-assignment and nested designation semantics remain conservative",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_csharp_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for CSharpAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/csharp/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.72,
            vec![
                "AST-driven local dataflow with conservative pattern subject-to-capture flow; structural projection and guard control dependencies remain conservative",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_csharp_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
        walk_csharp_pattern_edges(ctx.root, pos_map, edges);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction.
// ---------------------------------------------------------------------------

pub(crate) fn csharp_frontend() -> LanguageFrontend {
    let lang = Language::CSharp;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(CSharpAdapter),
        symbols: Box::new(CSharpAdapter),
        references: Box::new(CSharpAdapter),
        imports: Box::new(CSharpAdapter),
        scopes: Box::new(CSharpAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(CSharpAdapter),
        dataflow: Box::new(CSharpAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a C# symbol from parent namespace/class hierarchy.
fn qualified_name_from_node_csharp(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "enum_declaration" => {
                if let Some(type_name) = parent.child_by_field_name("name")
                    && let Ok(type_str) = type_name.utf8_text(source.as_bytes())
                {
                    parts.push(type_str.to_string());
                }
            }
            "namespace_declaration" => {
                if let Some(ns_name) = parent.child_by_field_name("name")
                    && let Ok(ns_str) = ns_name.utf8_text(source.as_bytes())
                {
                    parts.push(ns_str.to_string());
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

/// Map capture name to SymbolKind.
fn csharp_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.enum_member" => Some(SymbolKind::EnumMember),
        "definition.method" => Some(SymbolKind::Method),
        "definition.constructor" => Some(SymbolKind::Constructor),
        "definition.property" => Some(SymbolKind::Property),
        "definition.field" => Some(SymbolKind::Field),
        "definition.variable" => Some(SymbolKind::Variable),
        "definition.namespace" => Some(SymbolKind::Namespace),
        "definition.function" => Some(SymbolKind::Function), // delegate
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn csharp_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn csharp_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            // Last segment is the imported type/namespace name
            let name = text.rsplit('.').next().unwrap_or(&text).to_string();
            Some((ImportKind::Use, text, name))
        }
        _ => None,
    }
}

/// Extract method/constructor signature from the AST.
fn csharp_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.method" | "definition.constructor" => {
            let parent = node.parent()?;
            let declaration = node_text(parent, source)?;
            let header = declaration
                .split_once('{')
                .map(|(head, _)| head)
                .unwrap_or(declaration.as_str())
                .trim()
                .trim_end_matches(';')
                .trim();
            let name = node_text(node, source)?;
            let name_pos = header.find(&name)?;
            let before_name = header[..name_pos].trim();
            let after_name = header[name_pos + name.len()..].trim();
            let params = after_name
                .split_once("=>")
                .map(|(sig, _)| sig)
                .unwrap_or(after_name)
                .trim();
            if capture_name == "definition.constructor" {
                return compact_signature(params);
            }
            let return_type = before_name.split_whitespace().last();
            match return_type {
                Some(ret) if !ret.is_empty() => compact_signature(&format!("{params}: {ret}")),
                _ => compact_signature(params),
            }
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn csharp_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        "lexical.pattern" => Some(BindingKind::Local),
        _ => None,
    }
}

fn is_csharp_pattern_binding_node(node: tree_sitter::Node<'_>) -> bool {
    node.kind() == "identifier"
        && node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "declaration_pattern" | "recursive_pattern" | "var_pattern"
            ) && parent.child_by_field_name("name").is_some_and(|name| {
                name.start_byte() == node.start_byte() && name.end_byte() == node.end_byte()
            })
        })
}

fn collect_csharp_pattern_bindings<'tree>(
    node: tree_sitter::Node<'tree>,
    bindings: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if is_csharp_pattern_binding_node(node) {
        bindings.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_csharp_pattern_bindings(child, bindings);
    }
}

fn is_csharp_pattern_syntax(node: tree_sitter::Node<'_>) -> bool {
    node.kind() == "pattern" || node.kind().ends_with("_pattern")
}

fn connect_csharp_pattern_bindings(
    value: tree_sitter::Node<'_>,
    pattern_owner: tree_sitter::Node<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    let value_key = NodePosKey {
        start_byte: value.start_byte() as u32,
        end_byte: value.end_byte() as u32,
        kind: DataNodeKind::Expr,
    };
    let Some(&source_id) = pos_map.get(&value_key) else {
        return;
    };

    let mut targets = Vec::new();
    collect_csharp_pattern_bindings(pattern_owner, &mut targets);
    for target in targets {
        let target_key = NodePosKey {
            start_byte: target.start_byte() as u32,
            end_byte: target.end_byte() as u32,
            kind: DataNodeKind::Local,
        };
        let Some(&target_id) = pos_map.get(&target_key) else {
            continue;
        };
        if edges.iter().any(|edge| {
            edge.source == source_id
                && edge.target == target_id
                && edge.kind == DataFlowKind::Assign
        }) {
            continue;
        }
        let edge_id =
            DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
        edges.push(DataFlowEdge::new(
            edge_id,
            source_id,
            target_id,
            DataFlowKind::Assign,
            node_range(target),
            0.80,
        ));
    }
}

fn walk_csharp_pattern_edges(
    node: tree_sitter::Node<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    match node.kind() {
        "is_pattern_expression" => {
            if let (Some(value), Some(pattern)) = (
                node.child_by_field_name("expression"),
                node.child_by_field_name("pattern"),
            ) {
                connect_csharp_pattern_bindings(value, pattern, pos_map, edges);
            }
        }
        "switch_statement" => {
            if let Some(value) = node.child_by_field_name("value")
                && let Some(body) = node.child_by_field_name("body")
            {
                let mut cursor = body.walk();
                for section in body
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() == "switch_section")
                {
                    let mut section_cursor = section.walk();
                    for pattern in section
                        .named_children(&mut section_cursor)
                        .filter(|child| is_csharp_pattern_syntax(*child))
                    {
                        connect_csharp_pattern_bindings(value, pattern, pos_map, edges);
                    }
                }
            }
        }
        "switch_expression" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            if let Some(value) = children
                .iter()
                .copied()
                .find(|child| child.kind() != "switch_expression_arm")
            {
                for arm in children
                    .iter()
                    .copied()
                    .filter(|child| child.kind() == "switch_expression_arm")
                {
                    let mut arm_cursor = arm.walk();
                    for pattern in arm
                        .named_children(&mut arm_cursor)
                        .filter(|child| is_csharp_pattern_syntax(*child))
                    {
                        connect_csharp_pattern_bindings(value, pattern, pos_map, edges);
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_csharp_pattern_edges(child, pos_map, edges);
    }
}

fn normalize_csharp_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = csharp_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_csharp_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => make_df_parameter(file_id, node, source, range),
        "df.assign_target" | "df.pattern_target" => {
            make_df_assign_target(file_id, node, source, range)
        }
        "df.assign_value" | "df.pattern_value" => make_df_assign_value(
            file_id,
            node,
            source,
            range,
            &["invocation_expression", "object_creation_expression"],
        ),
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = name.clone();
                let callsite_id = crate::languages::shared::find_call_expression(
                    node,
                    &["invocation_expression", "object_creation_expression"],
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
        "df.call_arg" => make_df_call_arg(
            file_id,
            node,
            source,
            range,
            &["invocation_expression", "object_creation_expression"],
        ),
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "member_access_expression")
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
        "df.assign_field_target" => {
            // Node is a member_access_expression (e.g. "obj.Prop" or
            // "this.Field"). Create a Field DataNode with the full text
            // as name and access_path.
            let text = node_text(node, source).unwrap_or_default();
            make_df_assign_field_target(file_id, &text, range)
        }
        "df.await_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "expr",
                Some(&text),
                None,
                range.start_byte,
            );
            (
                Some(DataNode {
                    id: node_id,
                    file_id,
                    function_id: None,
                    kind: DataNodeKind::Expr,
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
        "df.receiver" | "df.literal" => {
            make_df_receiver_or_literal(file_id, capture_name, node, source, range)
        }
        "df.identifier_use" => {
            if is_csharp_pattern_binding_node(node) {
                return (None, None);
            }
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &["using_directive", "namespace_declaration"],
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
        let spec = CSharpAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = CSharpAdapter;
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
        let spec = CSharpAdapter;
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
        let spec = CSharpAdapter;
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
        let spec = CSharpAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = CSharpAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_field_assign_and_await() {
        let frontend = csharp_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void Bar() { obj.Prop = 42; var x = await DoAsync(); } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.cs");
        let ctx = NormalizeCtx {
            language: Language::CSharp,
            file_id,
            file_path: std::path::Path::new("Test.cs"),
            source,
        };

        let mut has_field = false;
        let mut has_expr = false; // await_value produces Expr nodes
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
                match dn.kind {
                    DataNodeKind::Field => has_field = true,
                    DataNodeKind::Expr => has_expr = true,
                    _ => {}
                }
            }
        }
        assert!(
            has_field,
            "should have a Field DataNode from obj.Prop assignment"
        );
        assert!(
            has_expr,
            "should have an Expr DataNode from await DoAsync()"
        );
    }

    #[test]
    fn test_pattern_bindings_keep_arm_identity_and_subject_flow() {
        let source = concat!(
            "class PatternDispatch {\n",
            "  static int Dispatch(object input) {\n",
            "    return input switch {\n",
            "      string value when value.Length > 0 => Consume(value),\n",
            "      int value => Consume(value),\n",
            "      Point { X: > 0 } point => Consume(point),\n",
            "      var fallback => Consume(fallback),\n",
            "    };\n",
            "  }\n",
            "}\n",
        );
        let file_id = FileId::generate("PatternDispatch.cs");
        let facts = crate::extract_file_with_mode(
            &csharp_frontend(),
            file_id,
            std::path::Path::new("PatternDispatch.cs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .expect("extract C# switch patterns");

        let mut value_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "value")
            .collect();
        value_bindings.sort_by_key(|binding| binding.range.start_byte);
        assert_eq!(value_bindings.len(), 2);
        assert_ne!(value_bindings[0].id, value_bindings[1].id);
        assert_ne!(value_bindings[0].scope_id, value_bindings[1].scope_id);
        assert!(
            value_bindings
                .iter()
                .all(|binding| binding.function_id.is_some())
        );

        for binding in &value_bindings {
            let uses: Vec<_> = facts
                .binding_uses
                .iter()
                .filter(|use_| use_.binding_id == Some(binding.id))
                .collect();
            assert!(uses.len() >= 2, "declaration and arm use: {uses:?}");
            assert!(
                uses.iter()
                    .all(|use_| use_.range.start_line == binding.range.start_line)
            );
        }

        let subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("input")
                    && node.range.start_line == 2
            })
            .expect("switch subject");
        let value_targets: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|node| {
                node.kind == DataNodeKind::Local && node.name.as_deref() == Some("value")
            })
            .collect();
        assert_eq!(value_targets.len(), 2);
        assert!(value_targets.iter().all(|target| {
            facts.dataflow_edges.iter().any(|edge| {
                edge.source == subject.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
            })
        }));
        assert!(
            value_targets
                .iter()
                .all(|target| target.binding_id.is_some())
        );
        for name in ["point", "fallback"] {
            let target = facts
                .data_nodes
                .iter()
                .find(|node| node.kind == DataNodeKind::Local && node.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} pattern target"));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == subject.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
            }));
            assert!(target.binding_id.is_some());
        }
    }

    #[test]
    fn test_is_pattern_binding_reaches_guard_and_true_body() {
        let source = concat!(
            "class PatternGuard {\n",
            "  static int Dispatch(object input) {\n",
            "    if (input is string text && text.Length > 0) {\n",
            "      return Consume(text);\n",
            "    }\n",
            "    return 0;\n",
            "  }\n",
            "}\n",
        );
        let file_id = FileId::generate("PatternGuard.cs");
        let facts = crate::extract_file_with_mode(
            &csharp_frontend(),
            file_id,
            std::path::Path::new("PatternGuard.cs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .expect("extract C# is-pattern");

        let binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "text")
            .expect("is-pattern binding");
        let scope = facts
            .scopes
            .iter()
            .find(|scope| scope.id == binding.scope_id)
            .expect("pattern scope");
        assert_eq!(scope.kind, ScopeKind::Conditional);
        let uses: Vec<_> = facts
            .binding_uses
            .iter()
            .filter(|use_| use_.name == "text")
            .collect();
        assert_eq!(uses.len(), 3, "declaration, guard, and body use: {uses:?}");
        assert!(uses.iter().all(|use_| use_.binding_id == Some(binding.id)));

        let subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("input")
                    && node.range.start_line == 2
            })
            .expect("is-pattern subject");
        let target = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Local
                    && node.name.as_deref() == Some("text")
                    && node.binding_id == Some(binding.id)
            })
            .expect("is-pattern target");
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == subject.id
                && edge.target == target.id
                && edge.kind == DataFlowKind::Assign
        }));
        assert!(
            facts.data_nodes.iter().all(|node| {
                node.kind != DataNodeKind::VariableUse || node.range != target.range
            })
        );
    }

    #[test]
    fn test_switch_statement_pattern_binding_receives_subject() {
        let source = concat!(
            "class PatternSwitch {\n",
            "  static int Dispatch(object input) {\n",
            "    switch (input) {\n",
            "      case string text when text.Length > 0:\n",
            "        return Consume(text);\n",
            "      default:\n",
            "        return 0;\n",
            "    }\n",
            "  }\n",
            "}\n",
        );
        let file_id = FileId::generate("PatternSwitch.cs");
        let facts = crate::extract_file_with_mode(
            &csharp_frontend(),
            file_id,
            std::path::Path::new("PatternSwitch.cs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .expect("extract C# switch statement pattern");

        let binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "text")
            .expect("switch-section binding");
        let uses: Vec<_> = facts
            .binding_uses
            .iter()
            .filter(|use_| use_.name == "text")
            .collect();
        assert_eq!(uses.len(), 3, "declaration, guard, and body use: {uses:?}");
        assert!(uses.iter().all(|use_| use_.binding_id == Some(binding.id)));

        let subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("input")
                    && node.range.start_line == 2
            })
            .expect("switch statement subject");
        let target = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Local
                    && node.name.as_deref() == Some("text")
                    && node.binding_id == Some(binding.id)
            })
            .expect("switch statement target");
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == subject.id
                && edge.target == target.id
                && edge.kind == DataFlowKind::Assign
        }));
    }

    #[test]
    fn test_nested_switch_patterns_keep_subject_ownership() {
        let source = concat!(
            "class NestedPattern {\n",
            "  static int Dispatch(object outer, object inner) {\n",
            "    return outer switch {\n",
            "      string outerText => inner switch {\n",
            "        int innerValue => Consume(innerValue),\n",
            "        _ => Consume(outerText),\n",
            "      },\n",
            "      _ => 0,\n",
            "    };\n",
            "  }\n",
            "}\n",
        );
        let file_id = FileId::generate("NestedPattern.cs");
        let facts = crate::extract_file_with_mode(
            &csharp_frontend(),
            file_id,
            std::path::Path::new("NestedPattern.cs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .expect("extract nested C# switch patterns");

        let node = |kind, name: &str, line| {
            facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == kind
                        && node.name.as_deref() == Some(name)
                        && node.range.start_line == line
                })
                .unwrap_or_else(|| panic!("missing {kind:?} {name} on line {line}"))
        };
        let outer_subject = node(DataNodeKind::Expr, "outer", 2);
        let inner_subject = node(DataNodeKind::Expr, "inner", 3);
        let outer_target = node(DataNodeKind::Local, "outerText", 3);
        let inner_target = node(DataNodeKind::Local, "innerValue", 4);
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == outer_subject.id
                && edge.target == outer_target.id
                && edge.kind == DataFlowKind::Assign
        }));
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == inner_subject.id
                && edge.target == inner_target.id
                && edge.kind == DataFlowKind::Assign
        }));
        assert!(
            facts
                .dataflow_edges
                .iter()
                .all(|edge| { edge.source != outer_subject.id || edge.target != inner_target.id })
        );
    }
}
