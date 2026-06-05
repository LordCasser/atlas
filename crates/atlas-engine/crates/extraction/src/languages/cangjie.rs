//! Cangjie frontend spec (slot-based).
//!
//! Provides query-driven extraction for Cangjie source files.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NoOpRecovery, NormalizeCtx, ParserSpec, ReferenceExtractorSpec,
    ScopeExtractorSpec, SymbolExtractorSpec,
};
use crate::languages::shared::{make_binding_def, make_reference_use, SymbolDefBuilder};
use types::bindings::BindingDef;
use types::capability::FeatureSupport;
use types::dataflow::{DataFlowEdge, DataNode};
use types::enums::{BindingKind, DataNodeKind};
use types::ids::{CallsiteId, DataNodeId, ScopeId};
use types::*;

/// Cangjie frontend spec.
pub(crate) struct CangjieAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — single source of truth for both the legacy
// slot trait impls only.
// ---------------------------------------------------------------------------

fn normalize_cangjie_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = cj_definition_kind(capture_name)?;
    // mainDefinition has no name child — its first child IS the entry
    // keyword token (TOKENS.MAIN = "main").  Extract text from child(0)
    // rather than the whole node (which would be "main(): Unit { ... }").
    let name = if capture_name == "definition.entry" {
        node.child(0)?
            .utf8_text(source.as_bytes())
            .ok()?
            .to_string()
    } else {
        node_text(node, source)?
    };
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_cj("", &name, node, source);

    Some(
        SymbolDefBuilder::new(
            file_id,
            Language::Cangjie,
            kind,
            name,
            qualified_name,
            range,
        )
        .build(),
    )
}

fn normalize_cangjie_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = cj_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    // source_symbol is resolved by SemanticBinder after extraction.
    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_cangjie_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
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

fn normalize_cangjie_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
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

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for CangjieAdapter {
    fn language(&self) -> Language {
        Language::Cangjie
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cangjie::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for CangjieAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/cangjie/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/cangjie/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_cangjie_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for CangjieAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/cangjie/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_cangjie_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for CangjieAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/cangjie/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_cangjie_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for CangjieAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/cangjie/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_cangjie_scope(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl LexicalBindingSpec for CangjieAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/cangjie/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.60,
            vec!["basic parameter/local binding extraction"],
        )
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_cangjie_lexical(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl DataflowSpec for CangjieAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/cangjie/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(0.60, vec!["AST-driven local dataflow"])
    }
    fn normalize(
        &self,
        _ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_cangjie_dataflow(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn cangjie_frontend() -> LanguageFrontend {
    let lang = Language::Cangjie;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(CangjieAdapter),
        symbols: Box::new(CangjieAdapter),
        references: Box::new(CangjieAdapter),
        imports: Box::new(CangjieAdapter),
        scopes: Box::new(CangjieAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(CangjieAdapter),
        dataflow: Box::new(CangjieAdapter),
        capability: cap,
        recovery: Box::new(NoOpRecovery),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        if parent.kind() == "classDefinition" {
            if let Some(child) = parent.child_by_field_name("className") {
                if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                    parts.push(class_name.to_string());
                }
            }
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
        "definition.entry" => Some(SymbolKind::Function), // mainDefinition
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

// ── Lexical binding normalize ──────────────────────────────────────────

fn cj_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        _ => None,
    }
}

fn normalize_cangjie_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = cj_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

/// Find the enclosing call expression (postfixExpression with callSuffix).
/// Checks the current node first, then walks up the parent chain.
fn find_call_expression_cangjie(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    // Check current node
    if is_cangjie_call_expr(node) {
        return Some(node);
    }
    // Walk up
    let mut current = node;
    while let Some(parent) = current.parent() {
        if is_cangjie_call_expr(parent) {
            return Some(parent);
        }
        current = parent;
    }
    None
}

fn is_cangjie_call_expr(node: tree_sitter::Node) -> bool {
    if node.kind() != "postfixExpression" {
        return false;
    }
    // Match both simple calls `func(args)` and method calls `obj.method(args)`.
    // Simple call:  postfixExpression(atomicVariable, callSuffix)
    // Method call:  postfixExpression(fieldAccess, callSuffix)
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "callSuffix" {
                // Also check that we have a callable target (atomicVariable or fieldAccess)
                let has_target = (0..node.child_count()).any(|j| {
                    node.child(j as u32)
                        .is_some_and(|c| c.kind() == "atomicVariable" || c.kind() == "fieldAccess")
                });
                if has_target {
                    return true;
                }
            }
        }
    }
    false
}

fn normalize_cangjie_dataflow(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
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
                let dn = DataNode::parameter(node_id, file_id, None, None, &name, range);
                (Some(dn), None)
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
                let dn = DataNode::local(node_id, file_id, None, None, &name, range);
                (Some(dn), None)
            })
            .unwrap_or((None, None)),
        "df.assign_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_cangjie(node)
                .map(|ce| CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
                callsite_id,
                name: Some(text),
                access_path: None,
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.return_value" => {
            // node is jumpExpression — find the expression child (last named child)
            let expr_node = (0..node.child_count())
                .rev()
                .find_map(|i| {
                    let c = node.child(i as u32)?;
                    if c.is_named() { Some(c) } else { None }
                })
                .unwrap_or(node);
            let text = node_text(expr_node, source).unwrap_or_default();
            let callsite_id = find_call_expression_cangjie(expr_node)
                .map(|ce| CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "return",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: DataNodeKind::Return,
                binding_id: None,
                callsite_id,
                name: Some(text),
                access_path: None,
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = name.clone();
                let callsite_id = find_call_expression_cangjie(node)
                    .map(|ce| CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_cangjie(node)
                .map(|ce| CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "call_arg",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode::call_arg(node_id, file_id, None, callsite_id, Some(&text), range);
            (Some(dn), None)
        }
        "df.field_name" | "df.receiver" => {
            let text = node_text(node, source).unwrap_or_default();
            let kind = if capture_name == "df.field_name" {
                DataNodeKind::Field
            } else {
                DataNodeKind::Receiver
            };
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                if capture_name == "df.field_name" {
                    "field"
                } else {
                    "receiver"
                },
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind,
                binding_id: None,
                callsite_id: None,
                name: Some(text.clone()),
                access_path: Some(text),
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.literal" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "literal",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: DataNodeKind::Literal,
                binding_id: None,
                callsite_id: None,
                name: Some(text),
                access_path: None,
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.identifier_use" => {
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
    fn test_cj_adapter_metadata() {
        let spec = CangjieAdapter;
        assert!(!spec.definition_query().is_empty());
        assert!(!spec.reference_query().is_empty());
        assert!(!spec.import_query().is_empty());
        assert!(!spec.scope_query().is_empty());
        assert!(!spec.lexical_query().is_empty());
        assert!(!spec.dataflow_builder_query().is_empty());
    }

    #[test]
    fn test_cj_queries_parse() {
        let spec = CangjieAdapter;
        let lang = spec.tree_sitter_language();

        // tree-sitter 0.26+ supports language ABI versions 13-15,
        // making Cangjie (ABI 15) fully compatible.
        let def_q = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(def_q.is_ok(), "definitions query: {:?}", def_q.err());

        let ref_q = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(ref_q.is_ok(), "references query: {:?}", ref_q.err());

        let imp_q = tree_sitter::Query::new(&lang, spec.import_query());
        assert!(imp_q.is_ok(), "imports query: {:?}", imp_q.err());

        let sc_q = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(sc_q.is_ok(), "scopes query: {:?}", sc_q.err());

        let lex_q = tree_sitter::Query::new(&lang, spec.lexical_query());
        assert!(lex_q.is_ok(), "lexical query: {:?}", lex_q.err());

        let df_q = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(df_q.is_ok(), "dataflow query: {:?}", df_q.err());
    }

    #[test]
    fn test_cj_definition_kind_mapping() {
        assert_eq!(
            cj_definition_kind("definition.class"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            cj_definition_kind("definition.function"),
            Some(SymbolKind::Function)
        );
        assert_eq!(cj_definition_kind("unknown"), None);
    }

    #[test]
    fn test_cj_reference_kind_mapping() {
        assert_eq!(
            cj_reference_kind("reference.call"),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            cj_reference_kind("reference.field"),
            Some(ReferenceKind::FieldAccess)
        );
        assert_eq!(cj_reference_kind("unknown"), None);
    }

    #[test]
    fn test_cj_import_info_mapping() {
        // import_info requires a real tree-sitter Node (from a parse tree)
        // — tests are deferred to E2E fixture-based tests.
    }

    #[test]
    fn test_cj_lexical_query_produces_bindings() {
        let lang: tree_sitter::Language = tree_sitter_cangjie::LANGUAGE.into();
        let query_src = include_str!("../../queries/cangjie/lexical.scm");
        let source = r#"func greet(name: String): String {
    let message = "Hello"
}
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&lang, query_src).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.cj");

        let mut bindings: Vec<BindingDef> = Vec::new();
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            if let Some(bd) = normalize_cangjie_lexical(&name, cap.node, source, file_id) {
                bindings.push(bd);
            }
        }

        assert_eq!(bindings.len(), 2, "expected 2 bindings, got {bindings:?}");
        assert!(
            bindings.iter().any(|b| b.kind == BindingKind::Parameter),
            "missing parameter binding"
        );
        assert!(
            bindings.iter().any(|b| b.kind == BindingKind::Local),
            "missing local binding"
        );
    }

    #[test]
    fn test_cj_dataflow_query_produces_nodes() {
        let lang: tree_sitter::Language = tree_sitter_cangjie::LANGUAGE.into();
        let query_src = include_str!("../../queries/cangjie/dataflow_builder.scm");
        let source = r#"func greet(name: String): String {
    let message = "Hello"
    return message
}
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&lang, query_src).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.cj");

        let mut nodes: Vec<DataNode> = Vec::new();
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            let (dn, _de) = normalize_cangjie_dataflow(&name, cap.node, source, file_id);
            if let Some(dn) = dn {
                nodes.push(dn);
            }
        }

        assert!(!nodes.is_empty(), "expected dataflow nodes, got none");
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::Parameter),
            "missing parameter node"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::Local),
            "missing local node"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::Return),
            "missing return node"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::Literal),
            "missing literal node"
        );
    }

    #[test]
    fn test_cj_dataflow_call_capture() {
        let lang: tree_sitter::Language = tree_sitter_cangjie::LANGUAGE.into();
        let query_src = include_str!("../../queries/cangjie/dataflow_builder.scm");
        let source = r#"func compute(x: Int64): Int64 {
    return add(x, 42)
}
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&lang, query_src).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.cj");

        let mut nodes: Vec<DataNode> = Vec::new();
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            let (dn, _de) = normalize_cangjie_dataflow(&name, cap.node, source, file_id);
            if let Some(dn) = dn {
                nodes.push(dn);
            }
        }

        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::CallTarget),
            "missing call target node"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::CallArg),
            "missing call arg node"
        );
        let call_target = nodes
            .iter()
            .find(|n| n.kind == DataNodeKind::CallTarget)
            .unwrap();
        assert!(
            call_target.callsite_id.is_some(),
            "call target should have callsite_id"
        );
        let call_args: Vec<_> = nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::CallArg)
            .collect();
        assert_eq!(call_args.len(), 2, "expected 2 call args");
        for arg in &call_args {
            assert!(
                arg.callsite_id.is_some(),
                "call arg should have callsite_id"
            );
        }
    }

    #[test]
    fn test_cj_binding_kind_mapping() {
        assert_eq!(
            cj_binding_kind("lexical.parameter"),
            Some(BindingKind::Parameter)
        );
        assert_eq!(cj_binding_kind("lexical.local"), Some(BindingKind::Local));
        assert_eq!(cj_binding_kind("unknown"), None);
    }

    #[test]
    fn test_cj_frontend_has_capability() {
        let frontend = cangjie_frontend();
        let cap = &frontend.capability;
        assert!(
            cap.supported_features
                .contains(&"lexical_bindings".to_string()),
            "should list lexical_bindings as supported, got: {:?}",
            cap.supported_features
        );
        assert!(
            cap.supported_features
                .contains(&"intra_statement_dataflow".to_string()),
            "should list intra_statement_dataflow as supported, got: {:?}",
            cap.supported_features
        );
    }
}
