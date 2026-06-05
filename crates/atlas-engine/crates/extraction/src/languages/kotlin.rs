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
    LexicalBindingSpec, NoOpRecovery, NormalizeCtx, ParserSpec, ReferenceExtractorSpec,
    ScopeExtractorSpec, SymbolExtractorSpec,
};
use crate::languages::shared::{make_binding_def, make_reference_use, SymbolDefBuilder};
use std::collections::HashMap;
use types::bindings::BindingDef;
use types::capability::FeatureSupport;
use types::dataflow::{DataFlowEdge, DataNode};
use types::enums::{DataFlowKind, DataNodeKind};
use types::ids::{DataFlowEdgeId, DataNodeId};
use types::structs::ScopeDef;
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
    let name = format!("{:?}#{}", kind, range.start_byte);
    let scope_path = name.clone();

    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);

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
            0.65,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_kotlin_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for KotlinAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/kotlin/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.65,
            vec!["AST-driven local dataflow with language-specific gaps"],
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

    // variable_declaration: val x = expr
    if kind == "variable_declaration" {
        let mut name_node: Option<tree_sitter::Node> = None;
        let mut value_node: Option<tree_sitter::Node> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.is_named() {
                    if child.kind() == "simple_identifier" && name_node.is_none() {
                        name_node = Some(child);
                    } else if value_node.is_none() {
                        value_node = Some(child);
                    }
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
            } else if !found_eq {
                if let Ok(t) = child.utf8_text(source.as_bytes()) {
                    if t == "=" {
                        found_eq = true;
                    }
                }
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
        recovery: Box::new(NoOpRecovery),
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
                if let Some(type_name) = parent.child_by_field_name("name") {
                    if let Ok(type_str) = type_name.utf8_text(source.as_bytes()) {
                        parts.push(type_str.to_string());
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
            // tree-sitter-kotlin v0.4.0 uses function_value_parameters
            if let Some(params) = parent.child_by_field_name("function_value_parameters") {
                Some(node_text(params, source)?)
            } else if let Some(params) = parent.child_by_field_name("parameters") {
                Some(node_text(params, source)?)
            } else {
                None
            }
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
        "df.parameter" => node_text(node, source)
            .map(|name| {
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "parameter",
                    Some(&name),
                    Some(&name),
                    range.start_byte,
                );
                (
                    Some(DataNode::parameter(
                        node_id, file_id, None, None, &name, range,
                    )),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.assign_target" => node_text(node, source)
            .map(|name| {
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "local",
                    Some(&name),
                    Some(&name),
                    range.start_byte,
                );
                (
                    Some(DataNode::local(node_id, file_id, None, None, &name, range)),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.assign_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id =
                crate::languages::shared::find_call_expression(node, &["call_expression"]).map(
                    |ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32),
                );
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
                    callsite_id,
                    name: Some(text),
                    access_path: None,
                    arg_index: None,
                    range,
                }),
                None,
            )
        }
        "df.return_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "return",
                Some(&text),
                None,
                range.start_byte,
            );
            (
                Some(DataNode {
                    id: node_id,
                    file_id,
                    function_id: None,
                    kind: DataNodeKind::Return,
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
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id =
                crate::languages::shared::find_call_expression(node, &["call_expression"]).map(
                    |ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32),
                );
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "call_arg",
                Some(&text),
                None,
                range.start_byte,
            );
            (
                Some(DataNode::call_arg(
                    node_id,
                    file_id,
                    None,
                    callsite_id,
                    Some(&text),
                    range,
                )),
                None,
            )
        }
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
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                if capture_name == "df.literal" {
                    "literal"
                } else {
                    "receiver"
                },
                Some(&text),
                None,
                range.start_byte,
            );
            (
                Some(DataNode {
                    id: node_id,
                    file_id,
                    function_id: None,
                    kind: if capture_name == "df.literal" {
                        DataNodeKind::Literal
                    } else {
                        DataNodeKind::Receiver
                    },
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
        "df.identifier_use" => {
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &["import_header", "package_header"],
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
        "df.assign_field_target" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "field",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode::field(node_id, file_id, None, &text, &text, range);
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
}
