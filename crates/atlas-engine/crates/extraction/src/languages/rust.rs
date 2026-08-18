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
            0.70,
            vec![
                "scope-chain-aware binding with arm-local match captures and source-ordered guard-let chains; syntactically ambiguous single-segment constants remain conservative",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_rust_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn is_binding_use(&self, node: tree_sitter::Node<'_>) -> bool {
        !is_rust_pattern_declaration_syntax(node)
    }
}

impl DataflowSpec for RustAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/rust/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.70,
            vec![
                "match scrutinees and guard-let values flow conservatively to arm-local captures; structural projection, borrow/move modes, and guard control dependencies remain conservative",
            ],
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
        walk_rust_language_edges(ctx.root, ctx.source, pos_map, edges);
        Ok(())
    }
}

/// Walk the AST for Rust-specific let bindings and match captures.
fn walk_rust_language_edges(
    node: tree_sitter::Node,
    source: &str,
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

    if node.kind() == "match_expression"
        && let (Some(subject), Some(body)) = (
            node.child_by_field_name("value"),
            node.child_by_field_name("body"),
        )
    {
        let subject_key = NodePosKey {
            start_byte: subject.start_byte() as u32,
            end_byte: subject.end_byte() as u32,
            kind: DataNodeKind::Expr,
        };
        if let Some(&source_id) = pos_map.get(&subject_key) {
            let mut cursor = body.walk();
            for arm in body
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "match_arm")
            {
                let Some(pattern) = arm.child_by_field_name("pattern") else {
                    continue;
                };
                let mut targets = Vec::new();
                collect_rust_match_binding_nodes(pattern, source, &mut targets);
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
                        node_range(target),
                        0.75,
                    ));
                }
            }
        }
    }

    if node.kind() == "let_condition"
        && rust_match_guard_let_condition(node).is_some()
        && let (Some(pattern), Some(value)) = (
            node.child_by_field_name("pattern"),
            node.child_by_field_name("value"),
        )
    {
        let value_key = NodePosKey {
            start_byte: value.start_byte() as u32,
            end_byte: value.end_byte() as u32,
            kind: DataNodeKind::Expr,
        };
        if let Some(&source_id) = pos_map.get(&value_key) {
            let mut targets = Vec::new();
            collect_rust_guard_let_binding_nodes(pattern, source, &mut targets);
            for target in targets {
                let target_key = NodePosKey {
                    start_byte: target.start_byte() as u32,
                    end_byte: target.end_byte() as u32,
                    kind: DataNodeKind::Local,
                };
                let Some(&target_id) = pos_map.get(&target_key) else {
                    continue;
                };
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
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_rust_language_edges(child, source, pos_map, edges);
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
        "lexical.pattern" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn node_is_within(node: tree_sitter::Node<'_>, ancestor: tree_sitter::Node<'_>) -> bool {
    node.start_byte() >= ancestor.start_byte() && node.end_byte() <= ancestor.end_byte()
}

fn nearest_ancestor_of_kind<'tree>(
    node: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    std::iter::successors(Some(node), |current| current.parent())
        .find(|ancestor| ancestor.kind() == kind)
}

fn rust_match_root_pattern(match_pattern: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let condition = match_pattern.child_by_field_name("condition");
    let mut cursor = match_pattern.walk();
    match_pattern.named_children(&mut cursor).find(|child| {
        condition.is_none_or(|condition| {
            child.start_byte() != condition.start_byte() || child.end_byte() != condition.end_byte()
        })
    })
}

fn is_rust_match_arm_pattern_syntax(node: tree_sitter::Node<'_>) -> bool {
    nearest_ancestor_of_kind(node, "match_pattern")
        .and_then(rust_match_root_pattern)
        .is_some_and(|pattern| node_is_within(node, pattern))
}

fn rust_match_guard_let_condition(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let let_condition = nearest_ancestor_of_kind(node, "let_condition")?;
    let match_pattern = nearest_ancestor_of_kind(let_condition, "match_pattern")?;
    let guard = match_pattern.child_by_field_name("condition")?;
    node_is_within(let_condition, guard).then_some(let_condition)
}

fn is_rust_guard_let_pattern_syntax(node: tree_sitter::Node<'_>) -> bool {
    rust_match_guard_let_condition(node)
        .and_then(|condition| condition.child_by_field_name("pattern"))
        .is_some_and(|pattern| node_is_within(node, pattern))
}

fn is_rust_pattern_declaration_syntax(node: tree_sitter::Node<'_>) -> bool {
    is_rust_match_arm_pattern_syntax(node) || is_rust_guard_let_pattern_syntax(node)
}

fn is_canonical_rust_or_alternative(
    node: tree_sitter::Node<'_>,
    root_pattern: tree_sitter::Node<'_>,
) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if ancestor.kind() == "or_pattern" {
            let Some(first) = ancestor.named_child(0) else {
                return false;
            };
            if !node_is_within(node, first) {
                return false;
            }
        }
        if ancestor.id() == root_pattern.id() {
            break;
        }
        current = ancestor.parent();
    }
    true
}

fn identifier_is_pattern_type_syntax(
    node: tree_sitter::Node<'_>,
    root_pattern: tree_sitter::Node<'_>,
) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if matches!(
            ancestor.kind(),
            "scoped_identifier" | "scoped_type_identifier" | "generic_pattern"
        ) {
            return true;
        }
        if matches!(ancestor.kind(), "tuple_struct_pattern" | "struct_pattern")
            && ancestor
                .child_by_field_name("type")
                .is_some_and(|type_node| node_is_within(node, type_node))
        {
            return true;
        }
        if ancestor.id() == root_pattern.id() {
            break;
        }
        current = ancestor.parent();
    }
    false
}

/// Classify a binding inside one Rust pattern root.
///
/// Rust requires every or-pattern alternative to bind the same names with the
/// same modes, so the first alternative is the canonical declaration site.
/// Upper-case bare identifiers remain conservatively classified as constant or
/// unit-variant syntax because tree-sitter cannot perform Rust name resolution.
fn is_rust_binding_node_in_pattern(
    node: tree_sitter::Node<'_>,
    source: &str,
    root_pattern: tree_sitter::Node<'_>,
) -> bool {
    if !matches!(node.kind(), "identifier" | "shorthand_field_identifier") {
        return false;
    }
    if !node_is_within(node, root_pattern) || !is_canonical_rust_or_alternative(node, root_pattern)
    {
        return false;
    }
    if node.kind() == "shorthand_field_identifier" {
        return true;
    }
    if identifier_is_pattern_type_syntax(node, root_pattern) {
        return false;
    }
    node_text(node, source)
        .and_then(|name| name.chars().next())
        .is_some_and(|first| !first.is_uppercase())
}

fn is_rust_match_binding_node(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let Some(match_pattern) = nearest_ancestor_of_kind(node, "match_pattern") else {
        return false;
    };
    let Some(root_pattern) = rust_match_root_pattern(match_pattern) else {
        return false;
    };
    is_rust_binding_node_in_pattern(node, source, root_pattern)
}

fn is_rust_guard_let_binding_node(node: tree_sitter::Node<'_>, source: &str) -> bool {
    rust_match_guard_let_condition(node)
        .and_then(|condition| condition.child_by_field_name("pattern"))
        .is_some_and(|pattern| is_rust_binding_node_in_pattern(node, source, pattern))
}

fn is_rust_pattern_binding_node(node: tree_sitter::Node<'_>, source: &str) -> bool {
    is_rust_match_binding_node(node, source) || is_rust_guard_let_binding_node(node, source)
}

fn collect_rust_match_binding_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &str,
    bindings: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if is_rust_match_binding_node(node, source) {
        bindings.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_rust_match_binding_nodes(child, source, bindings);
    }
}

fn collect_rust_guard_let_binding_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &str,
    bindings: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if is_rust_guard_let_binding_node(node, source) {
        bindings.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_rust_guard_let_binding_nodes(child, source, bindings);
    }
}

fn normalize_rust_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = rust_binding_kind(capture_name)?;
    if capture_name == "lexical.pattern" && !is_rust_pattern_binding_node(node, source) {
        return None;
    }
    let name = node_text(node, source)?;
    let range = node_range(node);
    let mut binding = make_binding_def(file_id, kind, name, range);
    if let Some(condition) = rust_match_guard_let_condition(node)
        && condition
            .child_by_field_name("pattern")
            .is_some_and(|pattern| node_is_within(node, pattern))
    {
        binding.visible_from_byte = condition.end_byte() as u32;
    }
    Some(binding)
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
        "df.pattern_target" => {
            if is_rust_pattern_binding_node(node, source) {
                make_df_assign_target(file_id, node, source, range)
            } else {
                (None, None)
            }
        }
        "df.assign_value" => {
            make_df_assign_value(file_id, node, source, range, &["call_expression"])
        }
        "df.match_subject" => {
            make_df_assign_value(file_id, node, source, range, &["call_expression"])
        }
        "df.guard_value" => {
            if rust_match_guard_let_condition(node).is_some() {
                make_df_assign_value(file_id, node, source, range, &["call_expression"])
            } else {
                (None, None)
            }
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
            if is_rust_pattern_declaration_syntax(node) {
                return (None, None);
            }
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

    #[test]
    fn test_rust_match_bindings_are_arm_scoped_and_receive_the_scrutinee() {
        let source = r#"enum Result {
    Good(i32),
    Bad(i32),
}
fn dispatch(value: Result) -> i32 {
    match value {
        Result::Good(payload) if payload > 0 => consume(payload),
        Result::Bad(payload) => match payload {
            inner => consume(inner),
        },
    }
}
"#;
        let file_id = FileId::generate("match_bindings.rs");
        let facts = crate::extract_file_with_mode(
            &rust_frontend(),
            file_id,
            std::path::Path::new("match_bindings.rs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let payload_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "payload")
            .collect();
        assert_eq!(
            payload_bindings.len(),
            2,
            "same-named captures in separate arms need distinct identities"
        );
        let inner_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "inner")
            .expect("nested match binding");
        for binding in payload_bindings
            .iter()
            .copied()
            .chain(std::iter::once(inner_binding))
        {
            let scope = facts
                .scopes
                .iter()
                .find(|scope| scope.id == binding.scope_id)
                .expect("binding scope");
            assert_eq!(scope.kind, ScopeKind::Conditional);
        }
        assert_ne!(payload_bindings[0].scope_id, payload_bindings[1].scope_id);

        let guarded_payload = payload_bindings
            .iter()
            .find(|binding| binding.range.start_line == 6)
            .expect("guarded payload binding");
        assert_eq!(
            facts
                .binding_uses
                .iter()
                .filter(|use_| use_.binding_id == Some(guarded_payload.id))
                .count(),
            3,
            "declaration, guard, and body must share one arm-local identity"
        );

        let outer_subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("value")
                    && node.range.start_line == 5
            })
            .expect("outer match scrutinee");
        for target in facts.data_nodes.iter().filter(|node| {
            node.kind == DataNodeKind::Local && node.name.as_deref() == Some("payload")
        }) {
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == outer_subject.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
            }));
        }

        let inner_subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("payload")
                    && node.range.start_line == 7
            })
            .expect("nested match scrutinee");
        let inner_target = facts
            .data_nodes
            .iter()
            .find(|node| node.kind == DataNodeKind::Local && node.name.as_deref() == Some("inner"))
            .expect("nested match target");
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == inner_subject.id
                && edge.target == inner_target.id
                && edge.kind == DataFlowKind::Assign
        }));
        assert!(
            facts
                .dataflow_edges
                .iter()
                .all(|edge| edge.source != outer_subject.id || edge.target != inner_target.id),
            "outer scrutinee must not flow into a nested match binding"
        );
    }

    #[test]
    fn test_rust_struct_ref_captured_and_or_patterns_use_canonical_bindings() {
        let source = r#"enum Message {
    Pair(i32, i32),
    Point { x: i32, y: i32 },
    Unit,
    Values([i32; 2]),
}
fn inspect(value: Message) -> i32 {
    match value {
        Message::Pair(left, ref right) if left > 0 => left + *right,
        Message::Point { x, y: renamed } => x + renamed,
        whole @ Message::Pair(_, inner) => consume(whole) + inner,
        Message::Pair(a, b) | Message::Pair(b, a) if a != b => a + b,
        Message::Values([first, mut second]) => first + second,
        Message::Unit => 0,
        lower::unit => 0,
    }
}
"#;
        let file_id = FileId::generate("structural_match_bindings.rs");
        let facts = crate::extract_file_with_mode(
            &rust_frontend(),
            file_id,
            std::path::Path::new("structural_match_bindings.rs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        for name in [
            "left", "right", "x", "renamed", "whole", "inner", "a", "b", "first", "second",
        ] {
            assert_eq!(
                facts
                    .bindings
                    .iter()
                    .filter(|binding| binding.name == name)
                    .count(),
                1,
                "{name} needs one canonical arm binding"
            );
        }
        for rejected in ["Message", "Pair", "Point", "Unit", "lower", "unit", "y"] {
            assert!(
                facts
                    .bindings
                    .iter()
                    .all(|binding| binding.name != rejected),
                "constructor/type/explicit-field syntax must not bind {rejected}"
            );
        }

        let subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("value")
                    && node.range.start_line == 7
            })
            .expect("match scrutinee");
        for name in [
            "left", "right", "x", "renamed", "whole", "inner", "a", "b", "first", "second",
        ] {
            let target = facts
                .data_nodes
                .iter()
                .find(|node| node.kind == DataNodeKind::Local && node.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("missing target {name}"));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == subject.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
            }));
        }

        for name in ["a", "b"] {
            let binding = facts
                .bindings
                .iter()
                .find(|binding| binding.name == name)
                .unwrap();
            assert_eq!(
                facts
                    .binding_uses
                    .iter()
                    .filter(|use_| use_.binding_id == Some(binding.id))
                    .count(),
                3,
                "one canonical declaration plus guard/body uses for {name}"
            );
        }
    }

    #[test]
    fn test_rust_match_guard_let_bindings_follow_source_order_and_value_flow() {
        let source = r#"fn inspect(value: Option<i32>, extra: Option<i32>) -> i32 {
    match value {
        Some(current)
            if before(extra)
                && let Some(extra) = extra
                && let Some(next) = Some(extra)
                && next > current
            => consume(extra) + next,
        _ => 0,
    }
}
"#;
        let file_id = FileId::generate("guard_let_bindings.rs");
        let facts = crate::extract_file_with_mode(
            &rust_frontend(),
            file_id,
            std::path::Path::new("guard_let_bindings.rs"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let extra_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "extra")
            .collect();
        assert_eq!(
            extra_bindings.len(),
            2,
            "parameter and guard-let shadow need distinct identities"
        );
        let outer_extra = extra_bindings
            .iter()
            .find(|binding| binding.kind == BindingKind::Parameter)
            .expect("outer parameter binding");
        let guard_extra = extra_bindings
            .iter()
            .find(|binding| binding.kind == BindingKind::Local)
            .expect("guard-let binding");
        let next = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "next")
            .expect("second guard-let binding");
        assert!(guard_extra.visible_from_byte > guard_extra.range.end_byte);
        assert!(next.visible_from_byte > next.range.end_byte);
        assert!(next.visible_from_byte > guard_extra.visible_from_byte);

        assert_eq!(
            facts
                .binding_uses
                .iter()
                .filter(|use_| use_.binding_id == Some(outer_extra.id))
                .count(),
            3,
            "parameter declaration, pre-let guard use, and guard RHS stay outer"
        );
        assert_eq!(
            facts
                .binding_uses
                .iter()
                .filter(|use_| use_.binding_id == Some(guard_extra.id))
                .count(),
            3,
            "guard declaration plus later let RHS and body share identity"
        );
        assert_eq!(
            facts
                .binding_uses
                .iter()
                .filter(|use_| use_.binding_id == Some(next.id))
                .count(),
            3,
            "second guard declaration, condition, and body share identity"
        );

        let guard_target = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Local && node.binding_id == Some(guard_extra.id)
            })
            .expect("guard-let Local target");
        let guard_rhs = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("extra")
                    && node.range.start_byte > guard_extra.range.end_byte
                    && node.range.start_byte < guard_extra.visible_from_byte
            })
            .expect("guard-let RHS value");
        assert_eq!(guard_rhs.binding_id, Some(outer_extra.id));
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == guard_rhs.id
                && edge.target == guard_target.id
                && edge.kind == DataFlowKind::Assign
        }));

        let later_extra_uses: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|node| {
                node.kind == DataNodeKind::VariableUse
                    && node.name.as_deref() == Some("extra")
                    && node.range.start_byte >= guard_extra.visible_from_byte
            })
            .collect();
        assert_eq!(later_extra_uses.len(), 2);
        assert!(
            later_extra_uses
                .iter()
                .all(|node| node.binding_id == Some(guard_extra.id))
        );
    }
}
