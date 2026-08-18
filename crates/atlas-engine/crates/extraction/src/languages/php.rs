//! PHP frontend spec (slot-based).
//!
//! Provides query-driven extraction for PHP source files.
//! Supports: class, interface, trait, enum, function, method, property, constant,
//! namespace definitions; function calls, method calls, static calls, object
//! creation, field access, type references; use/require/include imports; scopes.
//!
//! Special handling: `$` prefix stripped from variable/property names;
//! namespace separator is `\`.

use crate::languages::{node_range, node_text};
use crate::{dataflow_builder::NodePosKey, extraction_ctx::ExtractionCtx};
use std::collections::HashMap;

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::{
    SymbolDefBuilder, make_binding_def, make_df_assign_field_target, make_df_assign_value,
    make_df_receiver_or_literal, make_df_return_value, make_reference_use,
    make_scope_def_auto_name,
};
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// PHP frontend spec.
pub(crate) struct PhpAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_php_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = php_definition_kind(capture_name)?;
    let raw_name = node_text(node, source)?;
    // Strip `$` prefix from variable/property names
    let name = raw_name.trim_start_matches('$').to_string();
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_php("", &name, node, source);
    let signature = php_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Php, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_php_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = php_reference_kind(capture_name)?;
    let raw_name = node_text(node, source)?;
    // Strip `$` prefix for variable-like references (dynamic method names).
    let name = raw_name.trim_start_matches('$').to_string();
    let range = node_range(node);

    // Qualified calls capture the trailing `name` under `qualified_name`.
    // Mirror C++: name = last segment, text = full `\Foo\bar`, receiver = prefix.
    let (text, receiver) = if let Some(parent) = node.parent() {
        if parent.kind() == "qualified_name" {
            let full = node_text(parent, source).unwrap_or_else(|| name.clone());
            let recv = full
                .rsplit_once('\\')
                .map(|(prefix, _)| prefix.trim_end_matches('\\').to_string())
                .filter(|p| !p.is_empty());
            (full, recv)
        } else {
            (name.clone(), None)
        }
    } else {
        (name.clone(), None)
    };

    let mut r = make_reference_use(file_id, kind, text, name, range);
    if let Some(recv) = receiver {
        r.receiver = Some(recv);
    }
    Some(r)
}

fn normalize_php_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = php_import_info(capture_name, node, source)?;
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

fn normalize_php_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.namespace" => ScopeKind::Namespace,
        "scope.class" => ScopeKind::Class,
        "scope.interface" => ScopeKind::Interface,
        "scope.function" => ScopeKind::Function,
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

impl ParserSpec for PhpAdapter {
    fn language(&self) -> Language {
        Language::Php
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_php::LANGUAGE_PHP.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for PhpAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/php/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/php/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_php_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for PhpAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/php/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_php_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for PhpAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/php/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_php_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for PhpAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/php/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_php_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for PhpAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/php/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.62,
            vec![
                "scope-chain-aware file/function/method binding for parameters, assignment-created locals, []/list() nested/keyed/by-reference destructuring targets, foreach/catch/static declarations, and explicit anonymous-function captures; destructuring key expressions remain reads; global aliases, variable variables, non-variable destructuring targets, reference-alias semantics, and arrow-function ownership remain conservative",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_php_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn binding_use_query(&self) -> &str {
        "(variable_name) @binding.use"
    }

    fn normalize_binding_use_name(&self, raw: &str) -> String {
        strip_php_sigil(raw).to_string()
    }

    fn is_binding_use(&self, node: tree_sitter::Node<'_>) -> bool {
        php_enclosing_callable_kind(node) != Some("arrow_function")
    }

    fn coalesce_same_scope_bindings(&self) -> bool {
        true
    }

    fn is_lexical_scope(&self, kind: ScopeKind) -> bool {
        matches!(
            kind,
            ScopeKind::File | ScopeKind::Function | ScopeKind::Method
        )
    }

    fn inherits_bindings_from_parent(&self, scope: &ScopeDef) -> bool {
        !matches!(scope.kind, ScopeKind::Function | ScopeKind::Method)
    }
}

impl DataflowSpec for PhpAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/php/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.62,
            vec![
                "AST-driven local dataflow; assignment destructuring conservatively flows the whole RHS to each supported nested target (0.75), foreach collection flow reaches direct or nested key/value targets (0.65), and direct file/function/method variable augmented/update expressions preserve aggregate read-modify-write provenance (0.90); anonymous-function nodes remain in the enclosing named-function materialization unit; exact key/index projection, missing-key/null behavior, reference aliases, dynamic/non-variable mutation targets, conditional-write and prefix/postfix result timing, global aliases, variable variables, and arrow-function bodies remain conservative",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_php_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
        walk_php_collection_edges(ctx.root, pos_map, edges);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn php_frontend() -> LanguageFrontend {
    let lang = Language::Php;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(PhpAdapter),
        symbols: Box::new(PhpAdapter),
        references: Box::new(PhpAdapter),
        imports: Box::new(PhpAdapter),
        scopes: Box::new(PhpAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(PhpAdapter),
        dataflow: Box::new(PhpAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name using `\` as separator.
fn qualified_name_from_node_php(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "interface_declaration" | "trait_declaration" => {
                if let Some(type_name) = parent.child_by_field_name("name")
                    && let Ok(type_str) = type_name.utf8_text(source.as_bytes())
                {
                    parts.push(type_str.to_string());
                }
            }
            "namespace_definition" => {
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
        parts.join("\\")
    } else {
        format!("{}\\{}", prefix, parts.join("\\"))
    }
}

/// Map capture name to SymbolKind.
fn php_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.trait" => Some(SymbolKind::Trait),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.property" => Some(SymbolKind::Property),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.namespace" => Some(SymbolKind::Namespace),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn php_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn php_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let parent = node.parent()?;
            let kind = match parent.kind() {
                "require_expression" | "include_expression" => ImportKind::Include,
                _ => ImportKind::Use,
            };
            // For require/include: strip quotes
            let module = text.trim_matches(|c| c == '\'' || c == '"').to_string();
            let name = if kind == ImportKind::Use {
                module.rsplit('\\').next().unwrap_or(&module).to_string()
            } else {
                module.clone()
            };
            Some((kind, module, name))
        }
        _ => None,
    }
}

/// Extract function/method signature from the AST.
fn php_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" | "definition.method" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn php_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" | "lexical.destructure" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn normalize_php_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    if php_enclosing_callable_kind(node) == Some("arrow_function") {
        return None;
    }
    if capture_name == "lexical.destructure" && !is_php_list_destructure_target(node) {
        return None;
    }
    let kind = php_binding_kind(capture_name)?;
    let name = strip_php_sigil(&node_text(node, source)?).to_string();
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

/// Strip the `$` sigil from a PHP variable name so DataNode names are
/// consistent with definition/reference names (which already strip `$`).
fn strip_php_sigil(raw: &str) -> &str {
    raw.trim_start_matches('$')
}

/// Return the closest callable syntax enclosing `node`.
///
/// PHP arrow functions do not have a first-class symbol in the current
/// frontend. Their captures and parameters are therefore excluded instead of
/// being falsely attributed to an enclosing named function.
fn php_enclosing_callable_kind(node: tree_sitter::Node<'_>) -> Option<&'static str> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "arrow_function" => return Some("arrow_function"),
            "anonymous_function" => return Some("anonymous_function"),
            "function_definition" => return Some("function_definition"),
            "method_declaration" => return Some("method_declaration"),
            _ => current = parent.parent(),
        }
    }
    None
}

fn is_php_list_destructure_key_read(node: tree_sitter::Node<'_>) -> bool {
    let mut item = node;
    while let Some(parent) = item.parent() {
        if parent.kind() == "list_literal" {
            let mut sibling = item.next_sibling();
            while let Some(next) = sibling {
                if next.is_extra() {
                    sibling = next.next_sibling();
                    continue;
                }
                return next.kind() == "=>";
            }
            return false;
        }
        item = parent;
    }
    false
}

/// Whether a variable captured directly under a PHP `list_literal` is a target
/// rather than the key expression immediately before `=>`.
fn is_php_list_destructure_target(node: tree_sitter::Node<'_>) -> bool {
    let item = node
        .parent()
        .filter(|parent| parent.kind() == "by_ref")
        .unwrap_or(node);
    item.parent()
        .is_some_and(|parent| parent.kind() == "list_literal")
        && !is_php_list_destructure_key_read(node)
}

fn is_php_foreach_binding_target(node: tree_sitter::Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if current.kind() == "foreach_statement" {
            return php_foreach_parts(current).is_some_and(|(_, value)| {
                !is_php_list_destructure_key_read(node)
                    && value.start_byte() <= node.start_byte()
                    && value.end_byte() >= node.end_byte()
            });
        }
        parent = current.parent();
    }
    false
}

fn collect_php_destructure_targets<'tree>(
    node: tree_sitter::Node<'tree>,
    targets: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if node.kind() == "variable_name" {
        if node
            .parent()
            .is_some_and(|parent| parent.kind() == "list_literal")
            && !is_php_list_destructure_target(node)
        {
            return;
        }
        targets.push(node);
        return;
    }
    if !matches!(node.kind(), "by_ref" | "list_literal" | "pair") {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_php_destructure_targets(child, targets);
    }
}

fn connect_php_destructure_targets(
    value: tree_sitter::Node<'_>,
    target_owner: tree_sitter::Node<'_>,
    confidence: f64,
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
    collect_php_destructure_targets(target_owner, &mut targets);
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
            confidence,
        ));
    }
}

fn php_foreach_parts(
    node: tree_sitter::Node<'_>,
) -> Option<(tree_sitter::Node<'_>, tree_sitter::Node<'_>)> {
    let mut cursor = node.walk();
    let mut children = node
        .named_children(&mut cursor)
        .filter(|child| !child.is_extra());
    Some((children.next()?, children.next()?))
}

fn walk_php_collection_edges(
    node: tree_sitter::Node<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    if node.kind() == "assignment_expression"
        && let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        )
        && left.kind() == "list_literal"
    {
        connect_php_destructure_targets(right, left, 0.75, pos_map, edges);
    } else if node.kind() == "foreach_statement"
        && let Some((collection, value)) = php_foreach_parts(node)
    {
        connect_php_destructure_targets(collection, value, 0.65, pos_map, edges);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_php_collection_edges(child, pos_map, edges);
    }
}

fn normalize_php_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    if php_enclosing_callable_kind(node) == Some("arrow_function") {
        return (None, None);
    }
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => node_text(node, source)
            .map(|raw| {
                let name = strip_php_sigil(&raw).to_string();
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
        "df.assign_target"
        | "df.foreach_target"
        | "df.destructure_target"
        | "df.mutation_target" => {
            if capture_name == "df.destructure_target" && !is_php_list_destructure_target(node) {
                return (None, None);
            }
            node_text(node, source)
                .map(|raw| {
                    let name = strip_php_sigil(&raw).to_string();
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
                .unwrap_or((None, None))
        }
        "df.assign_value" | "df.destructure_value" | "df.foreach_value" | "df.mutation_value" => {
            make_df_assign_value(
                file_id,
                node,
                source,
                range,
                &[
                    "function_call_expression",
                    "member_call_expression",
                    "object_creation_expression",
                ],
            )
        }
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => node_text(node, source)
            .map(|raw_name| {
                // Strip $ sigil from variable_name nodes (dynamic method calls)
                // and name nodes alike for consistent naming.
                let name = strip_php_sigil(&raw_name).to_string();
                let access_path = name.clone();
                let callsite_id = crate::languages::shared::find_call_expression(
                    node,
                    &[
                        "function_call_expression",
                        "member_call_expression",
                        "object_creation_expression",
                    ],
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
            let name = strip_php_sigil(&text).to_string();
            let callsite_id = crate::languages::shared::find_call_expression(
                node,
                &[
                    "function_call_expression",
                    "member_call_expression",
                    "object_creation_expression",
                ],
            )
            .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "call_arg",
                Some(&name),
                None,
                range.start_byte,
            );
            (
                Some(DataNode::call_arg(
                    node_id,
                    file_id,
                    None,
                    callsite_id,
                    Some(&name),
                    range,
                )),
                None,
            )
        }
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| {
                        p.kind() == "member_access_expression" || p.kind() == "subscript_expression"
                    })
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
        // ── PHP dataflow additions (§2.11) ────────────────────────
        "df.index" => {
            // Index expression in $arr[$key] → Expr DataNode
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
        "df.assign_field_target" => {
            // Array assignment LHS: $arr[$key] = value → Field DataNode
            let text = node_text(node, source).unwrap_or_default();
            make_df_assign_field_target(file_id, &text, range)
        }
        "df.superglobal" => {
            // $_GET, $_POST, etc. → Global DataNode
            node_text(node, source)
                .map(|name| {
                    if name.starts_with("$_") {
                        let node_id = DataNodeId::generate(
                            &file_id,
                            None::<&SymbolId>,
                            "global",
                            Some(&name),
                            Some(&name),
                            range.start_byte,
                        );
                        (
                            Some(DataNode {
                                id: node_id,
                                file_id,
                                function_id: None,
                                kind: DataNodeKind::Global,
                                binding_id: None,
                                callsite_id: None,
                                name: Some(name),
                                access_path: None,
                                arg_index: None,
                                range,
                            }),
                            None,
                        )
                    } else {
                        (None, None)
                    }
                })
                .unwrap_or((None, None))
        }
        "df.identifier_use" | "df.mutation_read" => {
            if capture_name == "df.identifier_use" {
                // Filter out declaration contexts and superglobals
                if !is_php_list_destructure_key_read(node)
                    && crate::languages::shared::is_identifier_decl_or_property(
                        node,
                        &["namespace_use_clause", "use_declaration"],
                    )
                {
                    return (None, None);
                }
                // Skip left-hand side of assignment (already captured as df.assign_target)
                if let Some(parent) = node.parent()
                    && parent.kind() == "assignment_expression"
                    && parent
                        .child_by_field_name("left")
                        .is_some_and(|n| n.id() == node.id())
                {
                    return (None, None);
                }
                if is_php_list_destructure_target(node) || is_php_foreach_binding_target(node) {
                    return (None, None);
                }
            }
            // Skip superglobals (already captured as df.superglobal)
            let text = node_text(node, source).unwrap_or_default();
            if text.is_empty() || text.starts_with("$_") {
                return (None, None);
            }
            let name = strip_php_sigil(&text).to_string();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "identifier_use",
                Some(&name),
                Some(&name),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: DataNodeKind::VariableUse,
                binding_id: None,
                callsite_id: None,
                name: Some(name.clone()),
                access_path: Some(name),
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
        let spec = PhpAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = PhpAdapter;
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
        let spec = PhpAdapter;
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
        let spec = PhpAdapter;
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
        let spec = PhpAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = PhpAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_php() {
        let frontend = php_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = r#"<?php
function f($req) {
    $name = $_GET["name"];
    $clean = sanitize($name);
    return $clean;
}
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.php");
        let ctx = NormalizeCtx {
            language: Language::Php,
            file_id,
            file_path: std::path::Path::new("test.php"),
            source,
        };

        let mut has_global = false;
        let mut has_receiver = false;
        let mut has_expr = false;
        let mut has_parameter = false;
        let mut has_return = false;
        let mut has_local = false;
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
                    DataNodeKind::Global => has_global = true,
                    DataNodeKind::Receiver => has_receiver = true,
                    DataNodeKind::Expr => has_expr = true,
                    DataNodeKind::Parameter => has_parameter = true,
                    DataNodeKind::Return => has_return = true,
                    DataNodeKind::Local => has_local = true,
                    DataNodeKind::CallTarget => has_call_target = true,
                    _ => {}
                }
            }
        }
        assert!(has_global, "should have Global DataNode for $_GET");
        assert!(
            has_receiver,
            "should have Receiver DataNode for $_GET in $_GET['name']"
        );
        assert!(has_expr, "should have Expr DataNode for array index");
        assert!(has_parameter, "should have Parameter DataNode for $req");
        assert!(has_return, "should have Return DataNode");
        assert!(has_local, "should have Local DataNode for $name/$clean");
        assert!(
            has_call_target,
            "should have CallTarget DataNode for sanitize"
        );
    }

    #[test]
    fn test_foreach_bindings_share_function_namespace_after_loop() {
        let source = concat!(
            "<?php\n",
            "function iterate($items) {\n",
            "    foreach ($items as $key => $value) {\n",
            "        consume($value);\n",
            "    }\n",
            "    return $value + $key;\n",
            "}\n",
        );
        let file_id = FileId::generate("foreach_scope.php");
        let facts = crate::extract_file_with_mode(
            &php_frontend(),
            file_id,
            std::path::Path::new("foreach_scope.php"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let mut bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| matches!(binding.name.as_str(), "items" | "key" | "value"))
            .collect();
        bindings.sort_by_key(|binding| binding.name.as_str());
        assert_eq!(bindings.len(), 3);
        let function_scope = facts
            .scopes
            .iter()
            .find(|scope| scope.kind == ScopeKind::Function)
            .expect("PHP function scope");
        assert!(
            bindings
                .iter()
                .all(|binding| binding.scope_id == function_scope.id)
        );
        assert!(bindings.iter().all(|binding| binding.function_id.is_some()));

        let binding_by_name: std::collections::HashMap<_, _> = bindings
            .iter()
            .map(|binding| (binding.name.as_str(), binding.id))
            .collect();
        for (name, line) in [("value", 3), ("value", 5), ("key", 5), ("items", 2)] {
            let use_ = facts
                .binding_uses
                .iter()
                .find(|use_| use_.name == name && use_.range.start_line == line)
                .unwrap_or_else(|| panic!("PHP {name} use on line {line}"));
            assert_eq!(use_.binding_id, binding_by_name.get(name).copied());

            let node = facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == DataNodeKind::VariableUse
                        && node.name.as_deref() == Some(name)
                        && node.range.start_line == line
                })
                .unwrap_or_else(|| panic!("PHP {name} data node on line {line}"));
            assert_eq!(node.binding_id, binding_by_name.get(name).copied());
        }
    }

    #[test]
    fn test_nested_destructuring_bindings_and_conservative_flow() {
        let source = concat!(
            "<?php\n",
            "function unpack($source, $rows, $selector) {\n",
            "    [$first, [$left, &$right]] = $source;\n",
            "    ['id' => $id, 'meta' => ['flag' => $flag]] = $source;\n",
            "    list($a, list($b, $c)) = $source;\n",
            "    [$selector => $selected] = $source;\n",
            "    foreach ($rows as [$row_first, [$row_left, $row_right]]) {\n",
            "        consume($row_first, $row_left, $row_right);\n",
            "    }\n",
            "    foreach ($rows as ['id' => $row_id, 'meta' => ['flag' => $row_flag]]) {\n",
            "        consume($row_id, $row_flag);\n",
            "    }\n",
            "    foreach ($rows as $direct_key => &$direct_value) {\n",
            "        consume($direct_key, $direct_value);\n",
            "    }\n",
            "    return consume($first, $left, $right, $id, $flag, $a, $b, $c, $selected);\n",
            "}\n",
        );
        let facts = crate::extract_file_with_mode(
            &php_frontend(),
            FileId::generate("nested_destructure.php"),
            std::path::Path::new("nested_destructure.php"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let binding = |name: &str| {
            let matches: Vec<_> = facts
                .bindings
                .iter()
                .filter(|binding| binding.name == name)
                .collect();
            assert_eq!(matches.len(), 1, "expected one binding for {name}");
            matches[0]
        };
        for name in [
            "source",
            "rows",
            "selector",
            "first",
            "left",
            "right",
            "id",
            "flag",
            "a",
            "b",
            "c",
            "selected",
            "row_first",
            "row_left",
            "row_right",
            "row_id",
            "row_flag",
            "direct_key",
            "direct_value",
        ] {
            let binding = binding(name);
            let scope = facts
                .scopes
                .iter()
                .find(|scope| scope.id == binding.scope_id)
                .expect("binding scope");
            assert_eq!(scope.kind, ScopeKind::Function, "{name} function scope");
        }
        let selector_binding = binding("selector");
        assert_eq!(selector_binding.range.start_line, 1);
        assert!(
            !facts.data_nodes.iter().any(|node| {
                node.kind == DataNodeKind::Local
                    && node.name.as_deref() == Some("selector")
                    && node.range.start_line == 5
            }),
            "a keyed-destructuring selector is a read, not a target"
        );
        let selector_use = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::VariableUse
                    && node.name.as_deref() == Some("selector")
                    && node.range.start_line == 5
            })
            .expect("selector key read");
        assert_eq!(selector_use.binding_id, Some(selector_binding.id));

        let node = |kind: DataNodeKind, name: &str, line: u32| {
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
        let assert_flow = |source_name: &str, line: u32, target_name: &str, confidence: f64| {
            let source = node(DataNodeKind::Expr, source_name, line);
            let target = node(DataNodeKind::Local, target_name, line);
            let edge = facts
                .dataflow_edges
                .iter()
                .find(|edge| {
                    edge.source == source.id
                        && edge.target == target.id
                        && edge.kind == DataFlowKind::Assign
                })
                .unwrap_or_else(|| {
                    panic!("missing Assign flow {source_name}@{line} -> {target_name}@{line}")
                });
            assert_eq!(edge.confidence, confidence);
            assert_eq!(target.binding_id, Some(binding(target_name).id));
        };

        for target in ["first", "left", "right"] {
            assert_flow("$source", 2, target, 0.75);
        }
        for target in ["id", "flag"] {
            assert_flow("$source", 3, target, 0.75);
        }
        for target in ["a", "b", "c"] {
            assert_flow("$source", 4, target, 0.75);
        }
        assert_flow("$source", 5, "selected", 0.75);
        for target in ["row_first", "row_left", "row_right"] {
            assert_flow("$rows", 6, target, 0.65);
        }
        for target in ["row_id", "row_flag"] {
            assert_flow("$rows", 9, target, 0.65);
        }
        for target in ["direct_key", "direct_value"] {
            assert_flow("$rows", 12, target, 0.65);
        }
    }

    #[test]
    fn test_variable_mutations_preserve_read_modify_write_provenance() {
        let source = concat!(
            "<?php\n",
            "function mutate($seed, $delta) {\n",
            "    $total = $seed;\n",
            "    $total += $delta;\n",
            "    $total++;\n",
            "    --$total;\n",
            "    $items[0] += $delta;\n",
            "    $items[1]++;\n",
            "    return $total;\n",
            "}\n",
        );
        let facts = crate::extract_file_with_mode(
            &php_frontend(),
            FileId::generate("variable_mutations.php"),
            std::path::Path::new("variable_mutations.php"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let total_binding = {
            let matches: Vec<_> = facts
                .bindings
                .iter()
                .filter(|binding| binding.name == "total")
                .collect();
            assert_eq!(matches.len(), 1, "same-namespace writes must coalesce");
            matches[0]
        };
        let delta_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "delta")
            .expect("delta parameter binding");

        let node = |kind: DataNodeKind, name: &str, line: u32| {
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
        for (line, expression) in [(3, "$total += $delta"), (4, "$total++"), (5, "--$total")] {
            let target = node(DataNodeKind::Local, "total", line);
            let value = node(DataNodeKind::Expr, expression, line);
            let lhs_read = node(DataNodeKind::VariableUse, "total", line);
            assert_eq!(target.binding_id, Some(total_binding.id));
            assert_eq!(lhs_read.binding_id, Some(total_binding.id));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == value.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
                    && edge.confidence == 0.90
            }));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == lhs_read.id
                    && edge.target == value.id
                    && edge.kind == DataFlowKind::Read
                    && edge.confidence == 0.75
            }));
        }

        let rhs_read = node(DataNodeKind::VariableUse, "delta", 3);
        let compound_value = node(DataNodeKind::Expr, "$total += $delta", 3);
        assert_eq!(rhs_read.binding_id, Some(delta_binding.id));
        assert!(facts.dataflow_edges.iter().any(|edge| {
            edge.source == rhs_read.id
                && edge.target == compound_value.id
                && edge.kind == DataFlowKind::Read
                && edge.confidence == 0.75
        }));

        assert!(
            facts.data_nodes.iter().all(|node| {
                !(matches!(node.kind, DataNodeKind::Local | DataNodeKind::Expr)
                    && matches!(node.range.start_line, 6 | 7))
            }),
            "subscript mutations remain outside the variable-only boundary"
        );
    }

    #[test]
    fn test_assignment_bindings_are_isolated_by_anonymous_function_scope() {
        let source = concat!(
            "<?php\n",
            "function transform($input) {\n",
            "    $value = $input;\n",
            "    $hidden = $input;\n",
            "    $callback = function ($captured) use ($input) {\n",
            "        $value = $input + $captured;\n",
            "        consume($value, $hidden);\n",
            "    };\n",
            "    return $value;\n",
            "}\n",
        );
        let file_id = FileId::generate("anonymous_scope.php");
        let facts = crate::extract_file_with_mode(
            &php_frontend(),
            file_id,
            std::path::Path::new("anonymous_scope.php"),
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
        assert_ne!(value_bindings[0].id, value_bindings[1].id);
        assert_ne!(value_bindings[0].scope_id, value_bindings[1].scope_id);
        assert_eq!(value_bindings[0].function_id, value_bindings[1].function_id);
        assert!(value_bindings[0].function_id.is_some());

        let inner_value_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "value" && use_.range.start_line == 6)
            .expect("anonymous-function value use");
        assert_eq!(inner_value_use.binding_id, Some(value_bindings[1].id));
        let outer_value_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "value" && use_.range.start_line == 8)
            .expect("outer value use");
        assert_eq!(outer_value_use.binding_id, Some(value_bindings[0].id));

        let captured_input = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "input" && binding.range.start_line == 4)
            .expect("explicit anonymous-function capture");
        let inner_input_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "input" && use_.range.start_line == 5)
            .expect("captured input use");
        assert_eq!(inner_input_use.binding_id, Some(captured_input.id));

        let hidden_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "hidden" && use_.range.start_line == 6)
            .expect("non-captured anonymous-function use");
        assert_eq!(hidden_use.binding_id, None);
        let hidden_node = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::VariableUse
                    && node.name.as_deref() == Some("hidden")
                    && node.range.start_line == 6
            })
            .expect("non-captured anonymous-function data node");
        assert_eq!(hidden_node.binding_id, None);
    }

    #[test]
    fn test_arrow_function_facts_do_not_pollute_enclosing_function() {
        let source = concat!(
            "<?php\n",
            "function transform($input) {\n",
            "    $value = $input;\n",
            "    $callback = fn($value) => consume($value, $input);\n",
            "    return $value;\n",
            "}\n",
        );
        let file_id = FileId::generate("arrow_boundary.php");
        let facts = crate::extract_file_with_mode(
            &php_frontend(),
            file_id,
            std::path::Path::new("arrow_boundary.php"),
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
        assert_eq!(value_bindings.len(), 1, "only the outer assignment binds");
        assert_eq!(value_bindings[0].range.start_line, 2);
        assert!(
            facts.binding_uses.iter().all(|use_| {
                use_.range.start_line != 3 || !matches!(use_.name.as_str(), "value" | "input")
            }),
            "arrow parameters and captures must not inherit outer ownership"
        );
        assert!(
            facts.data_nodes.iter().all(|node| {
                node.range.start_line != 3
                    || (!matches!(node.name.as_deref(), Some("value" | "input" | "consume"))
                        && node.kind != DataNodeKind::Parameter)
            }),
            "arrow internals must not be attributed to the named function"
        );
    }
}
