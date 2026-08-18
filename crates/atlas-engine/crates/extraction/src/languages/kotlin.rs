//! Kotlin frontend spec (slot-based).
//!
//! Provides query-driven extraction for Kotlin source files.
//! Supports: class, object, function, property, variable, package definitions;
//! function calls, navigation access, type references; import resolution; scopes.
//!
//! Note: tree-sitter-kotlin v0.4.0 does not have separate interface/enum node
//! types — they share class_declaration with different body subtypes.

use crate::languages::{node_range, node_text};

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

/// Kotlin frontend spec.
pub(crate) struct KotlinAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_kotlin_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = kotlin_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_kotlin("", &name, node, source);
    let signature = kotlin_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Kotlin, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_kotlin_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = kotlin_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_kotlin_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = kotlin_import_info(capture_name, node, source)?;
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

fn normalize_kotlin_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.class" => ScopeKind::Class,
        "scope.interface" => ScopeKind::Interface,
        "scope.function" => ScopeKind::Function,
        "scope.block" => ScopeKind::Block,
        "scope.conditional" => ScopeKind::Conditional,
        "scope.loop" => ScopeKind::Loop,
        _ => return None,
    };
    let range = node_range(node);

    Some(make_scope_def_auto_name(file_id, kind, range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for KotlinAdapter {
    fn language(&self) -> Language {
        Language::Kotlin
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_kotlin::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for KotlinAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/kotlin/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/kotlin/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_kotlin_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for KotlinAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/kotlin/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_kotlin_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for KotlinAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/kotlin/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_kotlin_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for KotlinAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/kotlin/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_kotlin_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for KotlinAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/kotlin/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.67,
            vec![
                "scope-chain-aware parameter/local/catch binding with nested control-scope shadowing; extension receivers are not extracted by the pinned grammar and type-directed resolution is not modeled",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_kotlin_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn binding_use_query(&self) -> &str {
        "(simple_identifier) @binding.use"
    }

    fn is_binding_use(&self, node: tree_sitter::Node<'_>) -> bool {
        !is_kotlin_declaration_or_property_identifier(node)
    }
}

impl DataflowSpec for KotlinAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/kotlin/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.67,
            vec![
                "when subject initializers flow to scoped subject-variable bindings; smart-cast, definite-assignment, type/range projection, and guard control dependencies remain conservative",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_kotlin_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
        walk_kotlin_assign_edges(ctx.root, ctx.source, pos_map, edges);
        Ok(())
    }
}

/// Walk the AST for Kotlin-specific assignment patterns.
fn walk_kotlin_assign_edges(
    node: tree_sitter::Node,
    source: &str,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    let kind = node.kind();

    // when (val subject = initializer): initializer → subject
    if kind == "when_subject" {
        let declaration = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "variable_declaration");
        let name_node = declaration.and_then(|declaration| {
            declaration
                .named_children(&mut declaration.walk())
                .find(|child| child.kind() == "simple_identifier")
        });
        let value_node = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() != "variable_declaration");
        if let (Some(name), Some(value)) = (name_node, value_node) {
            let name_key = NodePosKey {
                start_byte: name.start_byte() as u32,
                end_byte: name.end_byte() as u32,
                kind: DataNodeKind::Local,
            };
            let value_key = NodePosKey {
                start_byte: value.start_byte() as u32,
                end_byte: value.end_byte() as u32,
                kind: DataNodeKind::Expr,
            };
            if let (Some(&target_id), Some(&source_id)) =
                (pos_map.get(&name_key), pos_map.get(&value_key))
            {
                let edge_id =
                    DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                edges.push(DataFlowEdge::new(
                    edge_id,
                    source_id,
                    target_id,
                    DataFlowKind::Assign,
                    node_range(name),
                    0.95,
                ));
            }
        }
    }

    // variable_declaration: val x = expr
    if kind == "variable_declaration" {
        let mut name_node: Option<tree_sitter::Node> = None;
        let mut value_node: Option<tree_sitter::Node> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32)
                && child.is_named()
            {
                if child.kind() == "simple_identifier" && name_node.is_none() {
                    name_node = Some(child);
                } else if value_node.is_none() {
                    value_node = Some(child);
                }
            }
        }
        if let (Some(name), Some(value)) = (name_node, value_node) {
            let name_key = NodePosKey {
                start_byte: name.start_byte() as u32,
                end_byte: name.end_byte() as u32,
                kind: DataNodeKind::Local,
            };
            let value_key = NodePosKey {
                start_byte: value.start_byte() as u32,
                end_byte: value.end_byte() as u32,
                kind: DataNodeKind::Expr,
            };
            if let (Some(&tid), Some(&sid)) = (pos_map.get(&name_key), pos_map.get(&value_key)) {
                let eid = DataFlowEdgeId::generate(&sid, &tid, DataFlowKind::Assign.as_str());
                edges.push(DataFlowEdge::new(
                    eid,
                    sid,
                    tid,
                    DataFlowKind::Assign,
                    node_range(name),
                    0.85,
                ));
            }
        }
    }

    // assignment: x = expr (children.multiple, no named fields)
    if kind == "assignment" {
        let mut target: Option<tree_sitter::Node> = None;
        let mut value: Option<tree_sitter::Node> = None;
        let mut found_eq = false;
        for i in 0..node.child_count() {
            let Some(child) = node.child(i as u32) else {
                continue;
            };
            if child.is_named() {
                if target.is_none() {
                    target = Some(child);
                } else if found_eq && value.is_none() {
                    value = Some(child);
                }
            } else if !found_eq
                && let Ok(t) = child.utf8_text(source.as_bytes())
                && t == "="
            {
                found_eq = true;
            }
        }
        if let (Some(t), Some(v)) = (target, value) {
            let t_key = NodePosKey {
                start_byte: t.start_byte() as u32,
                end_byte: t.end_byte() as u32,
                kind: DataNodeKind::Local,
            };
            let v_key = NodePosKey {
                start_byte: v.start_byte() as u32,
                end_byte: v.end_byte() as u32,
                kind: DataNodeKind::Expr,
            };
            if let (Some(&tid), Some(&sid)) = (pos_map.get(&t_key), pos_map.get(&v_key)) {
                let eid = DataFlowEdgeId::generate(&sid, &tid, DataFlowKind::Assign.as_str());
                edges.push(DataFlowEdge::new(
                    eid,
                    sid,
                    tid,
                    DataFlowKind::Assign,
                    node_range(t),
                    0.85,
                ));
            }
        }
    }

    // Recurse
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_kotlin_assign_edges(child, source, pos_map, edges);
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn kotlin_frontend() -> LanguageFrontend {
    let lang = Language::Kotlin;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(KotlinAdapter),
        symbols: Box::new(KotlinAdapter),
        references: Box::new(KotlinAdapter),
        imports: Box::new(KotlinAdapter),
        scopes: Box::new(KotlinAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(KotlinAdapter),
        dataflow: Box::new(KotlinAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name from parent class/object hierarchy.
fn qualified_name_from_node_kotlin(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "object_declaration" => {
                if let Some(type_name) = parent.child_by_field_name("name")
                    && let Ok(type_str) = type_name.utf8_text(source.as_bytes())
                {
                    parts.push(type_str.to_string());
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
fn kotlin_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class), // class + object
        "definition.function" => Some(SymbolKind::Function),
        "definition.property" => Some(SymbolKind::Property),
        "definition.variable" => Some(SymbolKind::Variable),
        "definition.package" => Some(SymbolKind::Package),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn kotlin_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn kotlin_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            // Last segment is the imported name
            let name = text.rsplit('.').next().unwrap_or(&text).to_string();
            Some((ImportKind::Import, text, name))
        }
        _ => None,
    }
}

/// Extract function signature from the AST.
fn kotlin_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" => {
            let parent = node.parent()?;
            let declaration = node_text(parent, source)?;
            let header = declaration
                .split_once('{')
                .map(|(head, _)| head)
                .unwrap_or(declaration.as_str())
                .split_once('=')
                .map(|(head, _)| head)
                .unwrap_or_else(|| {
                    declaration
                        .split_once('{')
                        .map(|(head, _)| head)
                        .unwrap_or(declaration.as_str())
                })
                .trim();
            let name = node_text(node, source)?;
            let name_pos = header.find(&name)?;
            compact_signature(header[name_pos + name.len()..].trim())
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn kotlin_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.receiver" => Some(BindingKind::Parameter), // extension function receiver → "this"
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn is_kotlin_declaration_or_property_identifier(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "variable_declaration" | "parameter" | "function_declaration" | "catch_block" => parent
            .named_children(&mut parent.walk())
            .find(|child| child.kind() == "simple_identifier")
            .is_some_and(|name| name.id() == node.id()),
        "navigation_suffix" | "package_header" | "import_header" => true,
        _ => crate::languages::shared::is_identifier_decl_or_property(
            node,
            &["variable_declaration", "package_header", "import_header"],
        ),
    }
}

fn normalize_kotlin_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = kotlin_binding_kind(capture_name)?;
    let range = node_range(node);
    // For extension function receivers, the binding name is "this"
    let name = if capture_name == "lexical.receiver" {
        "this".to_string()
    } else {
        node_text(node, source)?
    };
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_kotlin_dataflow_builder(
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
                    .filter(|p| p.kind() == "navigation_expression")
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
            if is_kotlin_declaration_or_property_identifier(node) {
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
        "df.assign_field_target" => {
            let text = node_text(node, source).unwrap_or_default();
            make_df_assign_field_target(file_id, &text, range)
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let spec = KotlinAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = KotlinAdapter;
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
        let spec = KotlinAdapter;
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
        let spec = KotlinAdapter;
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
        let spec = KotlinAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = super::kotlin_frontend();
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
        let frontend = kotlin_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = r#"fun foo(x: Int): Int {
    val y = x.field
    val result = bar(y, 42)
    return result
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

    #[test]
    fn test_when_subject_variable_binding_and_initializer_flow() {
        let source = concat!(
            "fun dispatch(source: Source): String {\n",
            "  return when (val result = source.load()) {\n",
            "    is Success if result.ready -> consume(result)\n",
            "    result -> echo(result)\n",
            "    is Failure -> fail(result.error)\n",
            "    else -> fallback(result)\n",
            "  }\n",
            "}\n",
        );
        let file_id = FileId::generate("when_subject.kt");
        let facts = crate::extract_file_with_mode(
            &kotlin_frontend(),
            file_id,
            std::path::Path::new("when_subject.kt"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let result_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "result")
            .expect("when subject binding");
        let result_scope = facts
            .scopes
            .iter()
            .find(|scope| scope.id == result_binding.scope_id)
            .expect("when subject scope");
        assert_eq!(result_scope.kind, ScopeKind::Conditional);

        let result_uses: Vec<_> = facts
            .binding_uses
            .iter()
            .filter(|use_| use_.name == "result")
            .collect();
        assert!(
            result_uses.len() >= 5,
            "declaration, guard, and branch bodies must be binding uses: {result_uses:?}"
        );
        assert!(
            result_uses
                .iter()
                .all(|use_| use_.binding_id == Some(result_binding.id))
        );
        assert!(
            result_uses.iter().any(|use_| use_.range.start_line == 3),
            "an explicit when condition must resolve the subject binding"
        );

        let target = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Local
                    && node.name.as_deref() == Some("result")
                    && node.range.start_line == 1
            })
            .expect("when subject Local");
        let initializer = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr && node.name.as_deref() == Some("source.load()")
            })
            .expect("when subject initializer Expr");
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == initializer.id
                && edge.target == target.id
                && edge.kind == DataFlowKind::Assign
        }));
        assert!(
            facts
                .data_nodes
                .iter()
                .filter(|node| {
                    node.kind == DataNodeKind::VariableUse && node.name.as_deref() == Some("result")
                })
                .all(|node| node.range != target.range),
            "the declaration name must not also be a VariableUse"
        );
        assert!(
            facts
                .data_nodes
                .iter()
                .filter(|node| {
                    node.kind == DataNodeKind::VariableUse && node.name.as_deref() == Some("result")
                })
                .all(|node| node.binding_id == Some(result_binding.id))
        );
    }

    #[test]
    fn test_nested_local_shadowing_keeps_scope_chain_identity() {
        let source = concat!(
            "fun shadow(input: Int): Int {\n",
            "  val value = input\n",
            "  if (input > 0) {\n",
            "    val value = input + 1\n",
            "    consume(value)\n",
            "  }\n",
            "  return value\n",
            "}\n",
        );
        let file_id = FileId::generate("shadow.kt");
        let facts = crate::extract_file_with_mode(
            &kotlin_frontend(),
            file_id,
            std::path::Path::new("shadow.kt"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let mut value_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "value")
            .collect();
        value_bindings.sort_by_key(|binding| binding.range.start_byte);
        assert_eq!(value_bindings.len(), 2);
        let outer = value_bindings[0];
        let inner = value_bindings[1];
        assert_ne!(outer.id, inner.id);
        assert_ne!(outer.scope_id, inner.scope_id);
        assert_eq!(outer.function_id, inner.function_id);
        assert!(outer.function_id.is_some());

        let inner_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "value" && use_.range.start_line == 4)
            .expect("inner consume(value) binding use");
        let outer_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "value" && use_.range.start_line == 6)
            .expect("outer return value binding use");
        assert_eq!(inner_use.binding_id, Some(inner.id));
        assert_eq!(outer_use.binding_id, Some(outer.id));

        let inner_node = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::VariableUse
                    && node.name.as_deref() == Some("value")
                    && node.range.start_line == 4
            })
            .expect("inner consume(value) data node");
        let outer_node = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::VariableUse
                    && node.name.as_deref() == Some("value")
                    && node.range.start_line == 6
            })
            .expect("outer return value data node");
        assert_eq!(inner_node.binding_id, Some(inner.id));
        assert_eq!(outer_node.binding_id, Some(outer.id));
    }
}
