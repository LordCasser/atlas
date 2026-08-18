//! Java frontend spec (slot-based).
//!
//! Provides query-driven extraction for Java source files.
//! Supports: class, interface, enum, method, field, constant, variable definitions;
//! method calls, field access, type references; import/include; scopes.

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

/// Java frontend spec.
pub(crate) struct JavaAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_java_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = java_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_java("", &name, node, source);
    let signature = java_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Java, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_java_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = java_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    // source_symbol is resolved by SemanticBinder after extraction.
    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_java_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
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

fn normalize_java_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
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

    Some(make_scope_def_auto_name(file_id, kind, range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for JavaAdapter {
    fn language(&self) -> Language {
        Language::Java
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for JavaAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/java/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/java/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_java_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for JavaAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/java/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_java_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for JavaAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/java/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_java_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for JavaAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/java/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_java_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for JavaAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/java/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.75,
            vec![
                "scope-chain-aware parameter/local/foreach/catch/lambda binding plus if-condition instanceof and arrow-switch type/record pattern captures; Java rejects overlapping local redeclaration, while sibling blocks and switch rules retain distinct identities; flow-sensitive boolean scope, colon-group patterns, and definite assignment remain conservative",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_java_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn is_binding_use(&self, node: tree_sitter::Node<'_>) -> bool {
        !is_java_pattern_binding_syntax(node)
    }
}

impl DataflowSpec for JavaAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/java/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.75,
            vec![
                "AST-driven local dataflow with conservative tested-value/selector flow to supported instanceof and arrow-switch pattern captures (0.75); exact record-component projection, flow-sensitive boolean scope, colon-group patterns, and guard control dependencies remain conservative",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_java_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
        walk_java_pattern_edges(ctx.root, pos_map, edges);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn java_frontend() -> LanguageFrontend {
    let lang = Language::Java;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(JavaAdapter),
        symbols: Box::new(JavaAdapter),
        references: Box::new(JavaAdapter),
        imports: Box::new(JavaAdapter),
        scopes: Box::new(JavaAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(JavaAdapter),
        dataflow: Box::new(JavaAdapter),
        capability: cap,
    })
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
                if let Some(child) = parent.child_by_field_name("name")
                    && let Ok(class_name) = child.utf8_text(source.as_bytes())
                {
                    parts.push(class_name.to_string());
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
                .split_once("throws")
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

/// Map capture name to BindingKind.
fn java_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        "lexical.pattern" => Some(BindingKind::Local),
        _ => None,
    }
}

fn same_java_node(left: tree_sitter::Node<'_>, right: tree_sitter::Node<'_>) -> bool {
    left.start_byte() == right.start_byte()
        && left.end_byte() == right.end_byte()
        && left.kind() == right.kind()
}

fn java_node_contains(ancestor: tree_sitter::Node<'_>, node: tree_sitter::Node<'_>) -> bool {
    ancestor.start_byte() <= node.start_byte() && ancestor.end_byte() >= node.end_byte()
}

fn java_instanceof_is_supported_if_condition(node: tree_sitter::Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if ancestor.kind() == "if_statement" {
            return ancestor
                .child_by_field_name("condition")
                .is_some_and(|condition| java_node_contains(condition, node));
        }
        if matches!(
            ancestor.kind(),
            "method_declaration"
                | "constructor_declaration"
                | "lambda_expression"
                | "class_declaration"
        ) {
            return false;
        }
        current = ancestor.parent();
    }
    false
}

fn is_java_pattern_binding_syntax(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "instanceof_expression" => parent
            .child_by_field_name("name")
            .is_some_and(|name| same_java_node(name, node)),
        "type_pattern" | "record_pattern_component" => parent
            .named_child_count()
            .checked_sub(1)
            .and_then(|index| parent.named_child(index as u32))
            .is_some_and(|name| same_java_node(name, node)),
        _ => false,
    }
}

fn java_pattern_binding_owner(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    if !is_java_pattern_binding_syntax(node) {
        return None;
    }

    let mut current = node.parent();
    while let Some(ancestor) = current {
        match ancestor.kind() {
            "switch_rule" => return Some(ancestor),
            "switch_block_statement_group" => return None,
            "instanceof_expression" => {
                return java_instanceof_is_supported_if_condition(ancestor).then_some(ancestor);
            }
            "method_declaration"
            | "constructor_declaration"
            | "lambda_expression"
            | "class_declaration" => return None,
            _ => current = ancestor.parent(),
        }
    }
    None
}

fn is_java_supported_pattern_subject(node: tree_sitter::Node<'_>) -> bool {
    node.parent().is_some_and(|parent| match parent.kind() {
        "instanceof_expression" => java_instanceof_is_supported_if_condition(parent),
        "parenthesized_expression" => parent
            .parent()
            .is_some_and(|ancestor| ancestor.kind() == "switch_expression"),
        _ => false,
    })
}

fn collect_java_pattern_bindings<'tree>(
    node: tree_sitter::Node<'tree>,
    targets: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if java_pattern_binding_owner(node).is_some() {
        targets.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_java_pattern_bindings(child, targets);
    }
}

fn connect_java_pattern_bindings(
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
    collect_java_pattern_bindings(pattern_owner, &mut targets);
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
            0.75,
        ));
    }
}

fn java_switch_subject(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let condition = node.child_by_field_name("condition")?;
    if condition.kind() != "parenthesized_expression" {
        return Some(condition);
    }
    let mut cursor = condition.walk();
    condition.named_children(&mut cursor).next()
}

fn walk_java_pattern_edges(
    node: tree_sitter::Node<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    match node.kind() {
        "instanceof_expression" if java_instanceof_is_supported_if_condition(node) => {
            if let Some(value) = node.child_by_field_name("left") {
                connect_java_pattern_bindings(value, node, pos_map, edges);
            }
        }
        "switch_expression" => {
            if let (Some(value), Some(body)) =
                (java_switch_subject(node), node.child_by_field_name("body"))
            {
                let mut cursor = body.walk();
                for rule in body
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() == "switch_rule")
                {
                    let mut rule_cursor = rule.walk();
                    if let Some(label) = rule
                        .named_children(&mut rule_cursor)
                        .find(|child| child.kind() == "switch_label")
                    {
                        connect_java_pattern_bindings(value, label, pos_map, edges);
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_java_pattern_edges(child, pos_map, edges);
    }
}

/// Normalize a lexical capture into a BindingDef.
fn normalize_java_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = java_binding_kind(capture_name)?;
    if capture_name == "lexical.pattern" && java_pattern_binding_owner(node).is_none() {
        return None;
    }
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

/// Find a call expression ancestor for a dataflow node.
/// Normalize a dataflow capture into (DataNode, DataFlowEdge).
fn normalize_java_dataflow_builder(
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
        "df.pattern_target" => {
            if java_pattern_binding_owner(node).is_some() {
                make_df_assign_target(file_id, node, source, range)
            } else {
                (None, None)
            }
        }
        "df.assign_value" => make_df_assign_value(
            file_id,
            node,
            source,
            range,
            &["method_invocation", "object_creation_expression"],
        ),
        "df.pattern_subject" => {
            if is_java_supported_pattern_subject(node) {
                make_df_assign_value(
                    file_id,
                    node,
                    source,
                    range,
                    &["method_invocation", "object_creation_expression"],
                )
            } else {
                (None, None)
            }
        }
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = name.clone();
                let callsite_id = crate::languages::shared::find_call_expression(
                    node,
                    &["method_invocation", "object_creation_expression"],
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
                let dn = DataNode::call_target(
                    node_id,
                    file_id,
                    None,
                    callsite_id,
                    &name,
                    &access_path,
                    range,
                );
                (Some(dn), None)
            })
            .unwrap_or((None, None)),
        "df.call_arg" => make_df_call_arg(
            file_id,
            node,
            source,
            range,
            &["method_invocation", "object_creation_expression"],
        ),
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "field_access")
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
                let dn = DataNode::field(node_id, file_id, None, &name, &access_path, range);
                (Some(dn), None)
            })
            .unwrap_or((None, None)),
        "df.assign_field_target" => {
            // Node is a field_access (e.g. "obj.field" or "this.field").
            // Create a Field DataNode with the full text as name and access_path.
            let text = node_text(node, source).unwrap_or_default();
            make_df_assign_field_target(file_id, &text, range)
        }
        "df.index" => {
            // Node is the index expression of an array access (e.g. arr[i]),
            // could be an identifier, literal, or complex expression.
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "expr",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
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
            };
            (Some(dn), None)
        }
        "df.receiver" | "df.literal" => {
            make_df_receiver_or_literal(file_id, capture_name, node, source, range)
        }
        "df.identifier_use" => {
            if is_java_pattern_binding_syntax(node) {
                return (None, None);
            }
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &[
                    "object_creation_expression",
                    "type_identifier",
                    "method_invocation",
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
        let spec = JavaAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = JavaAdapter;
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
        let spec = JavaAdapter;
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
        let spec = JavaAdapter;
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
        let spec = JavaAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_lexical_query_parses() {
        let spec = JavaAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.lexical_query());
        assert!(
            query.is_ok(),
            "lexical query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = JavaAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_lexical_normalize_parameter() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void bar(int x) {} }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&ts_lang, frontend.lexical.lexical_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut hits = 0;
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            if frontend
                .lexical
                .normalize(
                    ctx,
                    Capture {
                        name,
                        node: cap.node,
                    },
                )
                .is_some()
            {
                hits += 1;
            }
        }
        assert!(
            hits > 0,
            "lexical query should produce at least one BindingDef for int x parameter"
        );
    }

    #[test]
    fn test_dataflow_normalize_assignment() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { int bar() { int x = 1; x = 2; return x; } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut node_hits = 0;
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
            if dn.is_some() {
                node_hits += 1;
            }
        }
        assert!(
            node_hits > 0,
            "dataflow query should produce at least one DataNode for assignment + return"
        );
    }

    #[test]
    fn test_dataflow_call_arg_normalize() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void bar() { helper(x, 42); } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut has_call_arg = false;
        let mut has_call_target = false;
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
                    DataNodeKind::CallArg => has_call_arg = true,
                    DataNodeKind::CallTarget => has_call_target = true,
                    _ => {}
                }
            }
        }
        assert!(has_call_target, "should have a call target (helper)");
        assert!(has_call_arg, "should have call arguments (x, 42)");
    }

    #[test]
    fn test_dataflow_field_assignment_and_object_creation() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void bar() { obj.field = 42; Foo f = new Foo(x, y); } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut field_nodes: Vec<DataNode> = Vec::new();
        let mut call_target_nodes: Vec<DataNode> = Vec::new();
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
                    DataNodeKind::Field => field_nodes.push(dn),
                    DataNodeKind::CallTarget => call_target_nodes.push(dn),
                    _ => {}
                }
            }
        }

        // Should have at least one Field node from "obj.field" assignment
        assert!(
            !field_nodes.is_empty(),
            "should have at least one Field DataNode from obj.field assignment"
        );
        let field_texts: Vec<&str> = field_nodes
            .iter()
            .filter_map(|n| n.name.as_deref())
            .collect();
        assert!(
            field_texts.iter().any(|t| t.contains("field")),
            "should capture field assignment, got: {field_texts:?}"
        );

        // Should have a CallTarget DataNode from "new Foo()"
        let call_names: Vec<&str> = call_target_nodes
            .iter()
            .filter_map(|n| n.name.as_deref())
            .collect();
        assert!(
            call_names.iter().any(|t| t.contains("Foo")),
            "should capture new Foo(x, y) call target, got: {call_names:?}"
        );
    }

    #[test]
    fn test_pattern_bindings_are_scoped_and_receive_the_tested_value() {
        let source = concat!(
            "class PatternDispatch {\n",
            "  static int dispatch(Object input) {\n",
            "    if (input instanceof String text && !text.isEmpty()) {\n",
            "      return consume(text);\n",
            "    }\n",
            "    if (input instanceof Box(String label, Pair(Integer size, _)) && size > 0) {\n",
            "      return consume(label, size);\n",
            "    }\n",
            "    return switch (input) {\n",
            "      case String value when !value.isEmpty() -> consume(value);\n",
            "      case Integer value -> consume(value);\n",
            "      case Box(String name, Pair(Integer count, _)) -> consume(name, count);\n",
            "      default -> 0;\n",
            "    };\n",
            "  }\n",
            "  static int colonGroup(Object input) {\n",
            "    switch (input) {\n",
            "      case String legacy:\n",
            "        return consume(legacy);\n",
            "      default:\n",
            "        return 0;\n",
            "    }\n",
            "  }\n",
            "  static boolean detached(Object input) {\n",
            "    boolean matched = input instanceof String detached;\n",
            "    return matched;\n",
            "  }\n",
            "}\n",
        );
        let file_id = FileId::generate("PatternDispatch.java");
        let facts = crate::extract_file_with_mode(
            &java_frontend(),
            file_id,
            std::path::Path::new("PatternDispatch.java"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .expect("extract Java patterns");

        let pattern_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| {
                ["text", "label", "size", "value", "name", "count"].contains(&binding.name.as_str())
            })
            .collect();
        assert_eq!(
            pattern_bindings.len(),
            7,
            "direct and record pattern captures must become bindings: {pattern_bindings:?}"
        );
        assert!(
            pattern_bindings
                .iter()
                .all(|binding| binding.function_id.is_some())
        );
        assert!(facts.bindings.iter().all(|binding| {
            !["legacy", "detached", "Box", "Pair", "_"].contains(&binding.name.as_str())
        }));

        let mut value_bindings: Vec<_> = pattern_bindings
            .iter()
            .copied()
            .filter(|binding| binding.name == "value")
            .collect();
        value_bindings.sort_by_key(|binding| binding.range.start_byte);
        assert_eq!(value_bindings.len(), 2);
        assert_ne!(value_bindings[0].id, value_bindings[1].id);
        assert_ne!(value_bindings[0].scope_id, value_bindings[1].scope_id);

        for binding in &pattern_bindings {
            let uses: Vec<_> = facts
                .binding_uses
                .iter()
                .filter(|use_| use_.binding_id == Some(binding.id))
                .collect();
            assert!(
                uses.len() >= 2,
                "declaration and guard/body uses must share {}: {uses:?}",
                binding.name
            );
        }

        let subject_lines = [2, 5, 8];
        let subjects: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("input")
                    && subject_lines.contains(&node.range.start_line)
            })
            .collect();
        assert_eq!(subjects.len(), 3, "tested values and switch selector");

        for binding in pattern_bindings {
            let target = facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == DataNodeKind::Local && node.binding_id == Some(binding.id)
                })
                .unwrap_or_else(|| panic!("missing pattern target for {}", binding.name));
            let expected_subject_line = match binding.range.start_line {
                2 => 2,
                5 => 5,
                9..=11 => 8,
                line => panic!("unexpected pattern line {line}"),
            };
            let subject = subjects
                .iter()
                .copied()
                .find(|node| node.range.start_line == expected_subject_line)
                .expect("pattern subject");
            let edge = facts
                .dataflow_edges
                .iter()
                .find(|edge| {
                    edge.source == subject.id
                        && edge.target == target.id
                        && edge.kind == DataFlowKind::Assign
                })
                .unwrap_or_else(|| panic!("missing tested-value flow to {}", binding.name));
            assert_eq!(edge.confidence, 0.75);
            assert!(facts.data_nodes.iter().all(|node| {
                node.kind != DataNodeKind::VariableUse || node.range != target.range
            }));
            assert!(facts.dataflow_edges.iter().all(|candidate| {
                candidate.target != target.id
                    || candidate.kind != DataFlowKind::Assign
                    || candidate.source == subject.id
            }));
        }
    }
}
