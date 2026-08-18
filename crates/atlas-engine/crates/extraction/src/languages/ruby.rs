//! Ruby frontend spec (slot-based).
//!
//! Provides query-driven extraction for Ruby source files.
//! Supports: class, module, method, constant, variable, field (attr_*)
//! definitions; method calls, constant references; require/include/extend
//! imports; scopes.
//!
//! Note: singleton methods (`def self.method`) are captured as Method, not
//! differentiated from instance methods at the Symbolic level.
//!
//! ## Known gaps (documented, not yet implemented)
//!
//! - **Block/yield implicit calls**: `do |params| ... end` blocks passed to
//!   method calls are not modeled as virtual callsites.  `yield(args)` does
//!   not create dataflow edges to the calling context.  This means dataflow
//!   tracing stops at block boundaries and yield is treated as a sink.
//! - **Pattern projection**: `case/in` capture targets receive the whole match
//!   subject conservatively. Array/hash element projection and post-match
//!   path-definedness are not modeled.

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
    make_df_call_arg, make_df_parameter, make_df_receiver_or_literal, make_df_return_value,
    make_reference_use, make_scope_def_auto_name,
};
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Ruby frontend spec.
pub(crate) struct RubyAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_ruby_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = ruby_definition_kind(capture_name)?;
    let raw_name = node_text(node, source)?;
    // For symbols `:name`, strip the leading `:`
    let name = raw_name.trim_start_matches(':').to_string();
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_ruby("", &name, node, source);
    let signature = ruby_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Ruby, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_ruby_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = ruby_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_ruby_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = ruby_import_info(capture_name, node, source)?;
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

fn normalize_ruby_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.module" => ScopeKind::Module,
        "scope.class" => ScopeKind::Class,
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

impl ParserSpec for RubyAdapter {
    fn language(&self) -> Language {
        Language::Ruby
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_ruby::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for RubyAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/ruby/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/ruby/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_ruby_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for RubyAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/ruby/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_ruby_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for RubyAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/ruby/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_ruby_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for RubyAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/ruby/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_ruby_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for RubyAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/ruby/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.65,
            vec![
                "method/module/class local namespace identity; block assignment to an existing outer local remains conservative",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_ruby_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn coalesce_same_scope_bindings(&self) -> bool {
        true
    }

    fn is_lexical_scope(&self, kind: ScopeKind) -> bool {
        matches!(
            kind,
            ScopeKind::File
                | ScopeKind::Module
                | ScopeKind::Class
                | ScopeKind::Method
                | ScopeKind::Block
        )
    }

    fn binding_scope(
        &self,
        binding: &BindingDef,
        lexical_scopes: &[ScopeDef],
        preceding_bindings: &[BindingDef],
    ) -> ScopeId {
        ruby_binding_scope(binding, lexical_scopes, preceding_bindings)
    }
}

impl DataflowSpec for RubyAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/ruby/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.65,
            vec![
                "implicit return is approximate (body_statement last-child heuristic)",
                "method calls and field access share `call` node; attr_reader not resolved",
                "dynamic methods / method_missing not resolved",
                "case/in subjects flow conservatively to bare/as/rest/key-only captures; structural projection and post-match path-definedness remain path-insensitive",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_ruby_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
        walk_ruby_case_match_edges(ctx.root, pos_map, edges);
        Ok(())
    }
}

fn walk_ruby_case_match_edges(
    node: tree_sitter::Node<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    if node.kind() == "case_match"
        && let Some(subject) = node.child_by_field_name("value")
    {
        let subject_key = NodePosKey {
            start_byte: subject.start_byte() as u32,
            end_byte: subject.end_byte() as u32,
            kind: DataNodeKind::Expr,
        };
        if let Some(&source_id) = pos_map.get(&subject_key) {
            let mut cursor = node.walk();
            for clause in node
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "in_clause")
            {
                let Some(pattern) = clause.child_by_field_name("pattern") else {
                    continue;
                };
                let mut targets = Vec::new();
                collect_ruby_pattern_binding_nodes(pattern, &mut targets);
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

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ruby_case_match_edges(child, pos_map, edges);
    }
}

fn collect_ruby_pattern_binding_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    bindings: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if is_ruby_pattern_binding_node(node) {
        bindings.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_ruby_pattern_binding_nodes(child, bindings);
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn ruby_frontend() -> LanguageFrontend {
    let lang = Language::Ruby;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(RubyAdapter),
        symbols: Box::new(RubyAdapter),
        references: Box::new(RubyAdapter),
        imports: Box::new(RubyAdapter),
        scopes: Box::new(RubyAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(RubyAdapter),
        dataflow: Box::new(RubyAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name using `::` for modules/classes and `#` for methods.
fn qualified_name_from_node_ruby(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class" | "module" => {
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
        parts.join("::")
    } else {
        format!("{}::{}", prefix, parts.join("::"))
    }
}

/// Map capture name to SymbolKind.
fn ruby_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.module" => Some(SymbolKind::Module),
        "definition.method" => Some(SymbolKind::Method),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.variable" => Some(SymbolKind::Variable),
        "definition.field" => Some(SymbolKind::Field), // attr_*
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn ruby_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

/// Extract import info from capture.
fn ruby_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '\'' || c == '"').to_string();
            // Determine kind from ancestor call method name
            let method_name = find_ancestor_method_name(node, source)?;
            let kind = match method_name.as_str() {
                "require" | "require_relative" => ImportKind::Import,
                "include" | "extend" | "prepend" => ImportKind::Include,
                _ => ImportKind::Import,
            };
            Some((kind, cleaned.clone(), cleaned))
        }
        _ => None,
    }
}

/// Find the method name from the ancestor `call` node.
fn find_ancestor_method_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "call"
            && let Some(method) = parent.child_by_field_name("method")
        {
            return method
                .utf8_text(source.as_bytes())
                .ok()
                .map(|s| s.to_string());
        }
        current = parent.parent();
    }
    None
}

/// Extract method signature from the AST.
fn ruby_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.method" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn ruby_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        "lexical.pattern" => Some(BindingKind::Local),
        _ => None,
    }
}

/// Return the enclosing `in_clause` pattern when `node` is pattern syntax.
/// Guards and bodies are children of the same clause, so ancestor kind alone
/// is not sufficient to classify captures.
fn ruby_enclosing_pattern(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    std::iter::successors(node.parent(), |current| current.parent())
        .find(|ancestor| ancestor.kind() == "in_clause")
        .and_then(|clause| clause.child_by_field_name("pattern"))
        .filter(|pattern| {
            pattern.start_byte() <= node.start_byte() && pattern.end_byte() >= node.end_byte()
        })
}

fn is_ruby_pattern_syntax(node: tree_sitter::Node<'_>) -> bool {
    ruby_enclosing_pattern(node).is_some()
}

/// Select Ruby pattern nodes that write local variables. Pinned variables are
/// value reads, and hash keys bind only when their value sub-pattern is absent.
fn is_ruby_pattern_binding_node(node: tree_sitter::Node<'_>) -> bool {
    if !is_ruby_pattern_syntax(node) {
        return false;
    }

    match node.kind() {
        "hash_key_symbol" => node.parent().is_some_and(|parent| {
            parent.kind() == "keyword_pattern"
                && parent
                    .child_by_field_name("key")
                    .is_some_and(|key| key.id() == node.id())
                && parent.child_by_field_name("value").is_none()
        }),
        "identifier" => !std::iter::successors(node.parent(), |current| current.parent())
            .take_while(|ancestor| ancestor.kind() != "in_clause")
            .any(|ancestor| {
                matches!(
                    ancestor.kind(),
                    "variable_reference_pattern" | "expression_reference_pattern"
                )
            }),
        _ => false,
    }
}

fn ruby_pattern_binding_name(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    node_text(node, source).map(|name| name.trim_end_matches(':').to_string())
}

fn normalize_ruby_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = ruby_binding_kind(capture_name)?;
    if capture_name == "lexical.pattern" && !is_ruby_pattern_binding_node(node) {
        return None;
    }
    let name = if capture_name == "lexical.pattern" {
        ruby_pattern_binding_name(node, source)?
    } else {
        node_text(node, source)?
    };
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

/// Apply Ruby's source-ordered block-local namespace rule.
///
/// A block assignment reuses a same-named binding already established in an
/// ancestor lexical scope. Otherwise it introduces a block-local name. Block
/// parameters remain block-local even when they shadow an ancestor.
fn ruby_binding_scope(
    binding: &BindingDef,
    lexical_scopes: &[ScopeDef],
    preceding_bindings: &[BindingDef],
) -> ScopeId {
    if binding.kind != BindingKind::Local {
        return binding.scope_id;
    }
    let Some(current_scope) = lexical_scopes
        .iter()
        .find(|scope| scope.id == binding.scope_id)
    else {
        return binding.scope_id;
    };
    if current_scope.kind != ScopeKind::Block {
        return binding.scope_id;
    }

    if preceding_bindings
        .iter()
        .any(|prior| prior.scope_id == binding.scope_id && prior.name == binding.name)
    {
        return binding.scope_id;
    }

    let mut ancestors: Vec<_> = lexical_scopes
        .iter()
        .filter(|scope| {
            scope.id != current_scope.id
                && scope.range.start_byte <= current_scope.range.start_byte
                && scope.range.end_byte >= current_scope.range.end_byte
        })
        .collect();
    ancestors.sort_by_key(|scope| scope.range.byte_len());

    for ancestor in ancestors {
        if preceding_bindings
            .iter()
            .any(|prior| prior.scope_id == ancestor.id && prior.name == binding.name)
        {
            return ancestor.id;
        }
        if !matches!(ancestor.kind, ScopeKind::Block) {
            break;
        }
    }

    binding.scope_id
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_ruby_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => make_df_parameter(file_id, node, source, range),
        "df.pattern_target" => {
            if !is_ruby_pattern_binding_node(node) {
                return (None, None);
            }
            let Some(text) = ruby_pattern_binding_name(node, source) else {
                return (None, None);
            };
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "local",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            (
                Some(DataNode::local(node_id, file_id, None, None, &text, range)),
                None,
            )
        }
        "df.match_subject" => make_df_assign_value(file_id, node, source, range, &["call"]),
        "df.assign_target" => {
            // Differentiate by AST node kind: identifier → Local,
            // instance_variable (@x) → Field, class_variable (@@x) → Field,
            // global_variable ($x) → Global
            let text = node_text(node, source).unwrap_or_default();
            let dn = match node.kind() {
                "instance_variable" | "class_variable" => {
                    let node_id = DataNodeId::generate(
                        &file_id,
                        None::<&SymbolId>,
                        "field",
                        Some(&text),
                        Some(&text),
                        range.start_byte,
                    );
                    DataNode::field(node_id, file_id, None, &text, &text, range)
                }
                "global_variable" => {
                    let node_id = DataNodeId::generate(
                        &file_id,
                        None::<&SymbolId>,
                        "global",
                        Some(&text),
                        Some(&text),
                        range.start_byte,
                    );
                    DataNode {
                        id: node_id,
                        file_id,
                        function_id: None,
                        kind: DataNodeKind::Global,
                        binding_id: None,
                        callsite_id: None,
                        name: Some(text.clone()),
                        access_path: Some(text),
                        arg_index: None,
                        range,
                    }
                }
                _ => {
                    let node_id = DataNodeId::generate(
                        &file_id,
                        None::<&SymbolId>,
                        "local",
                        Some(&text),
                        Some(&text),
                        range.start_byte,
                    );
                    DataNode::local(node_id, file_id, None, None, &text, range)
                }
            };
            (Some(dn), None)
        }
        "df.assign_value" => make_df_assign_value(file_id, node, source, range, &["call"]),
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => {
            // The captured node is the `identifier` child of a `call` node.
            // Walk up to the parent `call` node and check for a `receiver`
            // to build a qualified name (e.g. "File.open").
            let terminal_text = node_text(node, source).unwrap_or_default();
            let (name, access_path) = node
                .parent()
                .filter(|p| p.kind() == "call")
                .and_then(|call_node| {
                    // Find the receiver text from the call node
                    let mut cursor = call_node.walk();
                    let receiver_text = call_node
                        .named_children(&mut cursor)
                        .find(|c| {
                            c.kind() == "constant"
                                || c.kind() == "identifier"
                                || c.kind() == "instance_variable"
                                || c.kind() == "class_variable"
                                || c.kind() == "global_variable"
                        })
                        .and_then(|r| node_text(r, source));
                    receiver_text.map(|recv| {
                        let qualified = format!("{recv}.{terminal_text}");
                        (qualified.clone(), qualified)
                    })
                })
                .unwrap_or_else(|| {
                    let t = terminal_text.clone();
                    (t.clone(), t)
                });
            let callsite_id = crate::languages::shared::find_call_expression(node, &["call"])
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
        }
        "df.call_arg" => make_df_call_arg(file_id, node, source, range, &["call"]),
        "df.field_name" => {
            // Build qualified access_path from receiver.method like df.call_target
            let terminal_text = node_text(node, source).unwrap_or_default();
            let (name, access_path) = node
                .parent()
                .filter(|p| p.kind() == "call")
                .and_then(|call_node| {
                    let mut cursor = call_node.walk();
                    let receiver_text = call_node
                        .named_children(&mut cursor)
                        .find(|c| {
                            c.kind() == "constant"
                                || c.kind() == "identifier"
                                || c.kind() == "instance_variable"
                                || c.kind() == "class_variable"
                                || c.kind() == "global_variable"
                        })
                        .and_then(|r| node_text(r, source));
                    receiver_text.map(|recv| {
                        let qualified = format!("{recv}.{terminal_text}");
                        (qualified.clone(), qualified)
                    })
                })
                .unwrap_or_else(|| {
                    let t = terminal_text.clone();
                    (t.clone(), t)
                });
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
        }
        "df.receiver" | "df.literal" => {
            make_df_receiver_or_literal(file_id, capture_name, node, source, range)
        }
        // ── Ruby dataflow additions (§2.12) ──────────────────────
        "df.implicit_return" => {
            // Query uses trailing `.` anchor: only the last child of
            // body_statement is captured, representing the implicit return.
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
        "df.identifier_use" => {
            if is_ruby_pattern_binding_node(node) {
                return (None, None);
            }
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &["class", "module", "method"],
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
        let spec = RubyAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = RubyAdapter;
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
        let spec = RubyAdapter;
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
        let spec = RubyAdapter;
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
        let spec = RubyAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = RubyAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_ruby() {
        let frontend = ruby_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source =
            "def f(params)\n  name = params[:name]\n  clean = sanitize(name)\n  clean\nend\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.rb");
        let ctx = NormalizeCtx {
            language: Language::Ruby,
            file_id,
            file_path: std::path::Path::new("test.rb"),
            source,
        };

        let mut has_parameter = false;
        let mut has_local = false;
        let mut has_field = false;
        let mut has_call_target = false;
        let mut has_implicit_return = false;
        let mut has_expr = false;
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
                    DataNodeKind::Parameter => has_parameter = true,
                    DataNodeKind::Local => has_local = true,
                    DataNodeKind::Field => has_field = true,
                    DataNodeKind::CallTarget => has_call_target = true,
                    DataNodeKind::Return => has_implicit_return = true,
                    DataNodeKind::Expr => has_expr = true,
                    _ => {}
                }
            }
        }
        assert!(has_parameter, "should have Parameter DataNode for params");
        assert!(has_local, "should have Local DataNode for name/clean");
        assert!(has_field, "should have Field DataNode for params[:name]");
        assert!(
            has_call_target,
            "should have CallTarget DataNode for sanitize"
        );
        assert!(
            has_implicit_return,
            "should have Return DataNode for implicit return (clean)"
        );
        assert!(has_expr, "should have Expr DataNode for assignment values");
    }

    #[test]
    fn test_case_match_pattern_bindings_and_subject_flow() {
        let source = concat!(
            "def dispatch(input, expected)\n",
            "  user = fallback\n",
            "  case input\n",
            "  in {user:, role: String, meta: {id: uid}, tags: [first, *rest]} => whole if allowed?(user, uid)\n",
            "    consume(user, uid, first, rest, whole)\n",
            "  in ^expected\n",
            "    pinned(expected)\n",
            "  end\n",
            "  use(user)\n",
            "end\n",
        );
        let file_id = FileId::generate("case_match.rb");
        let facts = crate::extract_file_with_mode(
            &ruby_frontend(),
            file_id,
            std::path::Path::new("case_match.rb"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let binding_names: Vec<_> = facts
            .bindings
            .iter()
            .map(|binding| binding.name.as_str())
            .collect();
        for expected in ["input", "expected", "user", "uid", "first", "rest", "whole"] {
            assert!(
                binding_names.contains(&expected),
                "missing binding {expected}"
            );
        }
        for rejected in ["role", "String", "allowed", "consume", "pinned"] {
            assert!(
                !binding_names.contains(&rejected),
                "value/key/call syntax must not bind {rejected}"
            );
        }
        assert_eq!(
            facts
                .bindings
                .iter()
                .filter(|binding| binding.name == "user")
                .count(),
            1,
            "assignment and case capture share the method-local binding"
        );

        let user_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "user")
            .unwrap();
        let user_scope = facts
            .scopes
            .iter()
            .find(|scope| scope.id == user_binding.scope_id)
            .unwrap();
        assert_eq!(user_scope.kind, ScopeKind::Method);
        assert!(
            facts
                .binding_uses
                .iter()
                .filter(|use_| use_.name == "user")
                .all(|use_| use_.binding_id == Some(user_binding.id))
        );

        let subject = facts
            .data_nodes
            .iter()
            .find(|node| {
                node.kind == DataNodeKind::Expr
                    && node.name.as_deref() == Some("input")
                    && node.range.start_line == 2
            })
            .expect("case subject expression");
        for name in ["user", "uid", "first", "rest", "whole"] {
            let target = facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == DataNodeKind::Local
                        && node.name.as_deref() == Some(name)
                        && node.range.start_line == 3
                })
                .unwrap_or_else(|| panic!("missing pattern target {name}"));
            assert!(facts.dataflow_edges.iter().any(|edge| {
                edge.source == subject.id
                    && edge.target == target.id
                    && edge.kind == DataFlowKind::Assign
            }));
        }
        assert!(!facts.data_nodes.iter().any(|node| {
            node.kind == DataNodeKind::Local
                && matches!(node.name.as_deref(), Some("role" | "expected"))
                && node.range.start_line >= 3
        }));
        assert!(
            facts
                .scopes
                .iter()
                .any(|scope| scope.kind == ScopeKind::Conditional),
            "case/in remains visible as structural scope"
        );
    }

    #[test]
    fn test_structural_control_scopes_share_ruby_local_namespace() {
        let source = concat!(
            "def scopes(flag)\n",
            "  begin\n",
            "    from_begin = 1\n",
            "  end\n",
            "  if flag\n",
            "    from_if = 2\n",
            "  end\n",
            "  while flag\n",
            "    from_loop = 3\n",
            "    break\n",
            "  end\n",
            "  1.times do\n",
            "    from_block = 4\n",
            "  end\n",
            "end\n",
        );
        let facts = crate::extract_file_with_mode(
            &ruby_frontend(),
            FileId::generate("scopes.rb"),
            std::path::Path::new("scopes.rb"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        for name in ["from_begin", "from_if", "from_loop"] {
            let binding = facts
                .bindings
                .iter()
                .find(|binding| binding.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            let scope = facts
                .scopes
                .iter()
                .find(|scope| scope.id == binding.scope_id)
                .unwrap();
            assert_eq!(scope.kind, ScopeKind::Method, "{name} must be method-local");
        }

        let block_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "from_block")
            .expect("block-local binding");
        let block_scope = facts
            .scopes
            .iter()
            .find(|scope| scope.id == block_binding.scope_id)
            .unwrap();
        assert_eq!(block_scope.kind, ScopeKind::Block);
        assert!(
            facts
                .scopes
                .iter()
                .any(|scope| scope.kind == ScopeKind::Conditional)
        );
        assert!(
            facts
                .scopes
                .iter()
                .any(|scope| scope.kind == ScopeKind::Loop)
        );
    }

    #[test]
    fn test_block_assignments_reuse_existing_ancestors_and_keep_new_locals() {
        let source = concat!(
            "def transform(input)\n",
            "  value = input\n",
            "  1.times do\n",
            "    value = value + 1\n",
            "    local = value\n",
            "    consume(value, local)\n",
            "  end\n",
            "  consume(value, local)\n",
            "end\n",
        );
        let facts = crate::extract_file_with_mode(
            &ruby_frontend(),
            FileId::generate("block_namespace.rb"),
            std::path::Path::new("block_namespace.rb"),
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
        assert_eq!(value_bindings.len(), 1, "block write reuses method local");
        let value_scope = facts
            .scopes
            .iter()
            .find(|scope| scope.id == value_bindings[0].scope_id)
            .expect("value scope");
        assert_eq!(value_scope.kind, ScopeKind::Method);
        assert!(
            facts
                .binding_uses
                .iter()
                .filter(|use_| use_.name == "value")
                .all(|use_| use_.binding_id == Some(value_bindings[0].id))
        );

        let local_binding = facts
            .bindings
            .iter()
            .find(|binding| binding.name == "local")
            .expect("new block local");
        let local_scope = facts
            .scopes
            .iter()
            .find(|scope| scope.id == local_binding.scope_id)
            .expect("local scope");
        assert_eq!(local_scope.kind, ScopeKind::Block);
        let inner_local_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "local" && use_.range.start_line == 5)
            .expect("inner local use");
        assert_eq!(inner_local_use.binding_id, Some(local_binding.id));
        let outer_local_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "local" && use_.range.start_line == 7)
            .expect("post-block local use");
        assert_eq!(outer_local_use.binding_id, None);

        for (line, expected) in [
            (3, Some(value_bindings[0].id)),
            (5, Some(value_bindings[0].id)),
        ] {
            let node = facts
                .data_nodes
                .iter()
                .find(|node| node.name.as_deref() == Some("value") && node.range.start_line == line)
                .unwrap_or_else(|| panic!("value data node on line {line}"));
            assert_eq!(node.binding_id, expected);
        }
    }

    #[test]
    fn test_block_namespace_is_source_ordered_and_parameters_shadow() {
        let source = concat!(
            "def scopes(input)\n",
            "  shadow = input\n",
            "  1.times do |shadow|\n",
            "    shadow = shadow + 1\n",
            "    consume(shadow)\n",
            "  end\n",
            "  1.times do\n",
            "    late = input\n",
            "    2.times do\n",
            "      late = late + 1\n",
            "      consume(late)\n",
            "    end\n",
            "  end\n",
            "  late = input\n",
            "  consume(late)\n",
            "end\n",
        );
        let facts = crate::extract_file_with_mode(
            &ruby_frontend(),
            FileId::generate("ordered_blocks.rb"),
            std::path::Path::new("ordered_blocks.rb"),
            source,
            "hash",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        let mut shadow_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "shadow")
            .collect();
        shadow_bindings.sort_by_key(|binding| binding.range.start_byte);
        assert_eq!(
            shadow_bindings.len(),
            2,
            "block parameter shadows method local"
        );
        assert_ne!(shadow_bindings[0].scope_id, shadow_bindings[1].scope_id);
        let shadow_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "shadow" && use_.range.start_line == 4)
            .expect("block shadow use");
        assert_eq!(shadow_use.binding_id, Some(shadow_bindings[1].id));

        let mut late_bindings: Vec<_> = facts
            .bindings
            .iter()
            .filter(|binding| binding.name == "late")
            .collect();
        late_bindings.sort_by_key(|binding| binding.range.start_byte);
        assert_eq!(
            late_bindings.len(),
            2,
            "later method assignment must not retroactively capture an earlier block local"
        );
        assert_ne!(late_bindings[0].scope_id, late_bindings[1].scope_id);
        let nested_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "late" && use_.range.start_line == 10)
            .expect("nested block use");
        assert_eq!(nested_use.binding_id, Some(late_bindings[0].id));
        let method_use = facts
            .binding_uses
            .iter()
            .find(|use_| use_.name == "late" && use_.range.start_line == 14)
            .expect("method-local use");
        assert_eq!(method_use.binding_id, Some(late_bindings[1].id));
    }
}
