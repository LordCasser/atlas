//! Go frontend spec (slot-based).
//!
//! Provides query-driven extraction for Go source files.
//! Supports: function, method, struct, interface, type_alias, variable, constant,
//! package definitions; function calls, field access, type references; import
//! resolution; scopes. Type-switch aliases use one binding per clause and
//! receive the guard value conservatively.

use crate::languages::{node_range, node_text};

use crate::dataflow_builder::{NodePosKey, create_assign_edges_from_expression_lists};
use crate::extraction_ctx::ExtractionCtx;
use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::{
    SymbolDefBuilder, make_binding_def, make_df_assign_field_target, make_df_assign_target,
    make_df_assign_value, make_df_call_arg, make_df_parameter, make_df_receiver_or_literal,
    make_df_return_value, make_reference_use, make_scope_def_auto_name,
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

/// Go frontend spec.
pub(crate) struct GoAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_go_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = go_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_go("", &name, node, source);
    let signature = go_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Go, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_go_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = go_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    // Extract receiver for method calls like `obj.Method()`.
    // The reference.scm query captures `field_identifier` inside
    // `selector_expression` → check parent for the operand.
    let receiver = if capture_name == "reference.call" {
        node.parent()
            .filter(|p| p.kind() == "selector_expression")
            .and_then(|sel| sel.child_by_field_name("operand"))
            .and_then(|op| node_text(op, source))
    } else {
        None
    };

    // source_symbol is resolved by SemanticBinder after extraction.
    let mut r = make_reference_use(file_id, kind, text, name, range);
    if let Some(recv) = receiver {
        r.receiver = Some(recv);
    }
    Some(r)
}

fn normalize_go_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = go_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let is_relative = module.starts_with('.');

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

fn normalize_go_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.function" => ScopeKind::Function,
        "scope.method" => ScopeKind::Method,
        "scope.block" => ScopeKind::Block,
        "scope.conditional" | "scope.type_switch_clause" => ScopeKind::Conditional,
        "scope.loop" => ScopeKind::Loop,
        _ => return None,
    };
    let range = node_range(node);

    Some(make_scope_def_auto_name(file_id, kind, range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for GoAdapter {
    fn language(&self) -> Language {
        Language::Go
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for GoAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/go/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/go/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_go_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for GoAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/go/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_go_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for GoAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/go/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_go_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for GoAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/go/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_go_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for GoAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/go/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.78,
            vec![
                "scope-chain-aware binding; type-switch aliases are clause-local, while short declarations with mixed new/existing names remain conservative",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_go_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn is_binding_use(&self, node: tree_sitter::Node<'_>) -> bool {
        !is_go_type_switch_alias_identifier(node)
    }
}

impl DataflowSpec for GoAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/go/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.78,
            vec![
                "AST-driven local dataflow; type-switch guard values flow to clause-local aliases, while case-type projection and mixed short-declaration identity remain conservative",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_go_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
        walk_go_assign_edges(ctx.root, pos_map, edges);
        Ok(())
    }
}

/// Walk the AST for Go-specific assignment patterns.
fn walk_go_assign_edges(
    node: tree_sitter::Node,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    let kind = node.kind();

    if kind == "type_switch_statement" {
        create_go_type_switch_edges(node, pos_map, edges);
    }

    // short_var_declaration: x := expr
    if kind == "short_var_declaration"
        && let (Some(left_list), Some(right_list)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        )
    {
        create_assign_edges_from_expression_lists(left_list, right_list, pos_map, edges);
    }

    // assignment_statement: x = expr
    if kind == "assignment_statement"
        && let (Some(left_list), Some(right_list)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        )
    {
        create_assign_edges_from_expression_lists(left_list, right_list, pos_map, edges);
    }

    // var_spec: var x = expr
    if kind == "var_spec"
        && let Some(name_node) = node.child_by_field_name("name")
    {
        let name_key = NodePosKey {
            start_byte: name_node.start_byte() as u32,
            end_byte: name_node.end_byte() as u32,
            kind: DataNodeKind::Local,
        };
        if let Some(val_list) = node.child_by_field_name("value") {
            for i in 0..val_list.child_count() {
                if let Some(val_node) = val_list.child(i as u32)
                    && val_node.is_named()
                {
                    let value_key = NodePosKey {
                        start_byte: val_node.start_byte() as u32,
                        end_byte: val_node.end_byte() as u32,
                        kind: DataNodeKind::Expr,
                    };
                    if let (Some(&target_id), Some(&source_id)) =
                        (pos_map.get(&name_key), pos_map.get(&value_key))
                    {
                        let edge_id = DataFlowEdgeId::generate(
                            &source_id,
                            &target_id,
                            DataFlowKind::Assign.as_str(),
                        );
                        edges.push(DataFlowEdge::new(
                            edge_id,
                            source_id,
                            target_id,
                            DataFlowKind::Assign,
                            node_range(name_node),
                            0.90,
                        ));
                    }
                }
            }
        }
    }

    // Recurse
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_go_assign_edges(child, pos_map, edges);
        }
    }
}

fn create_go_type_switch_edges(
    type_switch: tree_sitter::Node<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    if go_type_switch_alias_identifier(type_switch).is_none() {
        return;
    }
    let Some(subject) = type_switch.child_by_field_name("value") else {
        return;
    };
    let subject_key = NodePosKey {
        start_byte: subject.start_byte() as u32,
        end_byte: subject.end_byte() as u32,
        kind: DataNodeKind::Expr,
    };
    let Some(&source_id) = pos_map.get(&subject_key) else {
        return;
    };

    let mut cursor = type_switch.walk();
    for clause in type_switch
        .named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "type_case" | "default_case"))
    {
        let target_key = NodePosKey {
            start_byte: clause.start_byte() as u32,
            end_byte: clause.end_byte() as u32,
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
            node_range(subject),
            0.90,
        ));
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn go_frontend() -> LanguageFrontend {
    let lang = Language::Go;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(GoAdapter),
        symbols: Box::new(GoAdapter),
        references: Box::new(GoAdapter),
        imports: Box::new(GoAdapter),
        scopes: Box::new(GoAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(GoAdapter),
        dataflow: Box::new(GoAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a Go symbol from its parent hierarchy.
fn qualified_name_from_node_go(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    // Walk up to find enclosing type/function/package
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "type_declaration" => {
                // Find the type_spec containing our node to get the type name
                if let Some(type_spec) = find_ancestor_type_spec(&parent, current)
                    && let Some(type_name) = type_spec.child_by_field_name("name")
                    && let Ok(type_str) = type_name.utf8_text(source.as_bytes())
                {
                    parts.push(type_str.to_string());
                }
            }
            "source_file" => {
                // Extract package name from package_clause child.
                let mut cursor = parent.walk();
                for child in parent.children(&mut cursor) {
                    if child.kind() == "package_clause" {
                        let mut pkg_cursor = child.walk();
                        for pkg_child in child.children(&mut pkg_cursor) {
                            if pkg_child.kind() == "package_identifier" {
                                if let Ok(pkg) = pkg_child.utf8_text(source.as_bytes()) {
                                    parts.push(pkg.to_string());
                                }
                                break;
                            }
                        }
                        break;
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

/// Find the type_spec child that contains the given node.
fn find_ancestor_type_spec<'a>(
    type_decl: &tree_sitter::Node<'a>,
    target: tree_sitter::Node<'a>,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = type_decl.walk();
    for child in type_decl.children(&mut cursor) {
        if child.kind() == "type_spec" {
            // Check if target is within this type_spec's range
            if child.start_byte() <= target.start_byte() && target.end_byte() <= child.end_byte() {
                return Some(child);
            }
        }
    }
    None
}

/// Map capture name to SymbolKind.
fn go_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.class" => Some(SymbolKind::Struct), // struct in Go
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.type_alias" => Some(SymbolKind::TypeAlias),
        "definition.variable" => Some(SymbolKind::Variable),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.package" => Some(SymbolKind::Package),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn go_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

/// Extract import info from capture.
fn go_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            // Strip surrounding quotes from the string literal
            let path = text.trim_matches('"').to_string();
            // Last segment is a common local alias (but actual alias may differ)
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            Some((ImportKind::Import, path, name))
        }
        _ => None,
    }
}

/// Extract function/method signature (parameter list + return type) from the AST.
fn go_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" | "definition.method" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            let param_text = node_text(params, source)?;
            // Include return type if present
            if let Some(result) = parent.child_by_field_name("result") {
                let result_text = node_text(result, source)?;
                Some(format!("{param_text} {result_text}"))
            } else {
                Some(param_text)
            }
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn go_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        _ => None,
    }
}

fn go_type_switch_alias_identifier(
    type_switch: tree_sitter::Node<'_>,
) -> Option<tree_sitter::Node<'_>> {
    if type_switch.kind() != "type_switch_statement" {
        return None;
    }
    let alias_list = type_switch.child_by_field_name("alias")?;
    let mut cursor = alias_list.walk();
    let mut aliases = alias_list.named_children(&mut cursor);
    let alias = aliases.next()?;
    (alias.kind() == "identifier" && aliases.next().is_none()).then_some(alias)
}

fn is_go_type_switch_alias_identifier(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() != "identifier" {
        return false;
    }
    let Some(alias_list) = node
        .parent()
        .filter(|parent| parent.kind() == "expression_list")
    else {
        return false;
    };
    alias_list
        .parent()
        .and_then(go_type_switch_alias_identifier)
        .is_some_and(|alias| alias.id() == node.id())
}

/// Go's type-switch alias has no repeated identifier at each case boundary,
/// even though the language declares one distinct variable in every clause's
/// implicit block. Represent that implicit declaration as a zero-width source
/// point immediately before the clause body.
fn go_type_switch_clause_binding_range(clause: tree_sitter::Node<'_>) -> TextRange {
    let mut cursor = clause.walk();
    let body = clause
        .named_children(&mut cursor)
        .find(|child| child.kind() == "statement_list");
    let (byte, point) = body
        .map(|body| (body.start_byte(), body.start_position()))
        .unwrap_or_else(|| (clause.end_byte(), clause.end_position()));
    TextRange {
        start_byte: byte as u32,
        end_byte: byte as u32,
        start_line: point.row as u32,
        start_column: point.column as u32,
        end_line: point.row as u32,
        end_column: point.column as u32,
    }
}

fn normalize_go_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    if capture_name == "lexical.type_switch_clause" {
        let type_switch = node
            .parent()
            .filter(|parent| parent.kind() == "type_switch_statement")?;
        let alias = go_type_switch_alias_identifier(type_switch)?;
        let name = node_text(alias, source)?;
        return Some(make_binding_def(
            file_id,
            BindingKind::Local,
            name,
            go_type_switch_clause_binding_range(node),
        ));
    }
    let kind = go_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_go_dataflow_builder(
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
        "df.type_switch_subject" => {
            make_df_assign_value(file_id, node, source, range, &["call_expression"])
        }
        "df.type_switch_target" => {
            let Some(type_switch) = node
                .parent()
                .filter(|parent| parent.kind() == "type_switch_statement")
            else {
                return (None, None);
            };
            let Some(alias) = go_type_switch_alias_identifier(type_switch) else {
                return (None, None);
            };
            let Some(name) = node_text(alias, source) else {
                return (None, None);
            };
            let binding_range = go_type_switch_clause_binding_range(node);
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "local",
                Some(&name),
                Some(&name),
                binding_range.start_byte,
            );
            (
                Some(DataNode::local(
                    node_id,
                    file_id,
                    None,
                    None,
                    &name,
                    binding_range,
                )),
                None,
            )
        }
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => {
            // The captured node is field_identifier (e.g., "Open") inside a
            // selector_expression (e.g., "os.Open").  Walk up to the parent
            // selector_expression to get the full qualified callee text.
            // For unqualified calls the node is a plain identifier (e.g., "Println").
            let terminal_text = node_text(node, source).unwrap_or_default();
            let name = node
                .parent()
                .filter(|p| p.kind() == "selector_expression")
                .and_then(|p| node_text(p, source))
                .unwrap_or_else(|| terminal_text.clone());
            let access_path = name.clone();
            let callsite_id =
                crate::languages::shared::find_call_expression(node, &["call_expression"]).map(
                    |ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32),
                );
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
        }
        "df.call_arg" => make_df_call_arg(file_id, node, source, range, &["call_expression"]),
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "selector_expression")
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
        "df.receiver" | "df.literal" => {
            make_df_receiver_or_literal(file_id, capture_name, node, source, range)
        }
        "df.identifier_use" => {
            if is_go_type_switch_alias_identifier(node) {
                return (None, None);
            }
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &["type_declaration", "type_spec"],
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
        let spec = GoAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = GoAdapter;
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
        let spec = GoAdapter;
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
        let spec = GoAdapter;
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
        let spec = GoAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = super::go_frontend();
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
        let frontend = go_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = r#"package main
func foo(x int) int {
	y := x.field
	result := bar(y, 42)
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
    fn test_type_switch_aliases_are_clause_local_and_receive_the_guard_value() {
        let source = r#"package p
func dispatch(value any) any {
	switch value := value.(type) {
	case string:
		return consume(value)
	case []byte, nil:
		return consume(value)
	default:
		return consume(value)
	}
	return value
}
"#;
        let file_id = FileId::generate("type_switch.go");
        let facts = crate::extract_file_with_mode(
            &go_frontend(),
            file_id,
            std::path::Path::new("type_switch.go"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let value_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "value")
            .collect();
        assert_eq!(
            value_bindings.len(),
            4,
            "parameter plus one implicit alias binding per clause"
        );
        let outer = value_bindings
            .iter()
            .find(|binding| binding.kind == BindingKind::Parameter)
            .expect("outer parameter binding");
        let dispatch_id = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "dispatch")
            .expect("dispatch function")
            .id;
        assert!(
            value_bindings
                .iter()
                .all(|binding| binding.function_id == Some(dispatch_id)),
            "function-scoped extraction and Focus materialization must agree"
        );
        let mut clause_bindings: Vec<_> = value_bindings
            .iter()
            .filter(|binding| binding.kind == BindingKind::Local)
            .copied()
            .collect();
        clause_bindings.sort_by_key(|binding| binding.range.start_byte);
        assert_eq!(clause_bindings.len(), 3);
        assert_eq!(
            clause_bindings
                .iter()
                .map(|binding| binding.range.start_line)
                .collect::<Vec<_>>(),
            vec![4, 6, 8],
            "implicit declaration points start each clause body"
        );
        assert!(clause_bindings.iter().all(|binding| {
            binding.range.start_byte == binding.range.end_byte
                && binding.visible_from_byte == binding.range.start_byte
                && facts.scopes.iter().any(|scope| {
                    scope.id == binding.scope_id && scope.kind == ScopeKind::Conditional
                })
        }));
        assert_eq!(
            clause_bindings
                .iter()
                .map(|binding| binding.scope_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "every case/default clause has a distinct implicit block"
        );

        let guard_uses: Vec<_> = facts
            .binding_uses
            .iter()
            .filter(|use_| use_.name == "value" && use_.range.start_line == 2)
            .collect();
        assert_eq!(
            guard_uses.len(),
            1,
            "the declaration alias itself is not a read"
        );
        assert_eq!(guard_uses[0].binding_id, Some(outer.id));
        for (line, binding) in [
            (4, clause_bindings[0]),
            (6, clause_bindings[1]),
            (8, clause_bindings[2]),
        ] {
            assert!(facts.binding_uses.iter().any(|use_| {
                use_.name == "value"
                    && use_.range.start_line == line
                    && use_.binding_id == Some(binding.id)
            }));
        }
        assert!(facts.binding_uses.iter().any(|use_| {
            use_.name == "value" && use_.range.start_line == 10 && use_.binding_id == Some(outer.id)
        }));

        let subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("value")
                    && node.range.start_line == 2
            })
            .expect("type-switch guard value");
        assert_eq!(subject.binding_id, Some(outer.id));
        for binding in clause_bindings {
            let target = facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == DataNodeKind::Local
                        && node.name.as_deref() == Some("value")
                        && node.binding_id == Some(binding.id)
                })
                .expect("clause-local alias target");
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == subject.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
            }));
        }
    }

    #[test]
    fn test_type_switch_without_alias_does_not_invent_bindings_or_targets() {
        let source = r#"package p
func observe(value any) {
	switch value.(type) {
	case string:
		consume(value)
	default:
		consume(value)
	}
}
"#;
        let file_id = FileId::generate("type_switch_no_alias.go");
        let facts = crate::extract_file_with_mode(
            &go_frontend(),
            file_id,
            std::path::Path::new("type_switch_no_alias.go"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        assert_eq!(
            facts
                .bindings
                .iter()
                .filter(|binding| binding.name == "value")
                .count(),
            1,
            "only the function parameter is a binding"
        );
        assert!(!facts.data_nodes.iter().any(|node| {
            node.kind == DataNodeKind::Local && node.name.as_deref() == Some("value")
        }));
    }
}
