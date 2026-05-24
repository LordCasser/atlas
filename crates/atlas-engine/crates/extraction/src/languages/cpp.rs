//! C++ frontend spec (slot-based).
//!
//! Provides query-driven extraction for C++ source files.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::SymbolDefBuilder;
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// C++ frontend spec.
pub(crate) struct CppAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_cpp_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = cpp_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_cpp(&name, node, source);
    let signature = cpp_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Cpp, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_cpp_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = cpp_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    let ref_id = ReferenceId::generate(
        &file_id,
        None::<&SymbolId>,
        range.start_byte,
        range.end_byte,
        &text,
        kind,
    );

    // source_symbol is resolved by SemanticBinder after extraction.
    Some(ReferenceUse {
        id: ref_id,
        file_id,
        source_symbol: None,
        scope_id: None,
        kind,
        text,
        name,
        receiver: None,
        arity: None,
        range,
        resolved: None,
        binding_id: None,
    })
}

fn normalize_cpp_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = cpp_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let is_relative = !module.starts_with('<');

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
        imported_name,
        local_name: None,
        is_wildcard: false,
        is_relative,
        range,
        alias: None,
    })
}

fn normalize_cpp_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = cpp_scope_kind(capture_name)?;
    let name = node_text(node, source).unwrap_or_default();
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

impl ParserSpec for CppAdapter {
    fn language(&self) -> Language {
        Language::Cpp
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cpp::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for CppAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/cpp/definitions.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_cpp_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for CppAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/cpp/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_cpp_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for CppAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/cpp/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_cpp_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for CppAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/cpp/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_cpp_scope(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl LexicalBindingSpec for CppAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/cpp/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(0.55, vec!["name-based binding (no proper shadowing)"])
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_cpp_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for CppAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/cpp/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(0.65, vec!["AST-driven local dataflow with language-specific gaps"])
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_cpp_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn cpp_frontend() -> LanguageFrontend {
    let lang = Language::Cpp;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(CppAdapter),
        symbols: Box::new(CppAdapter),
        references: Box::new(CppAdapter),
        imports: Box::new(CppAdapter),
        scopes: Box::new(CppAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(CppAdapter),
        dataflow: Box::new(CppAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn qualified_name_from_node_cpp(name: &str, node: tree_sitter::Node, source: &str) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_specifier" | "struct_specifier" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(class_name.to_string());
                    }
                }
            }
            "namespace_definition" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(ns_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(ns_name.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }

    parts.reverse();
    parts.join("::")
}

fn cpp_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.class" => Some(SymbolKind::Class),
        "definition.namespace" => Some(SymbolKind::Namespace),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.macro" => Some(SymbolKind::Macro),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

fn cpp_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

fn cpp_scope_kind(capture: &str) -> Option<ScopeKind> {
    match capture {
        "scope.file" => Some(ScopeKind::File),
        "scope.function" => Some(ScopeKind::Function),
        "scope.class" => Some(ScopeKind::Class),
        "scope.namespace" => Some(ScopeKind::Namespace),
        "scope.block" => Some(ScopeKind::Block),
        "scope.conditional" => Some(ScopeKind::Conditional),
        "scope.loop" => Some(ScopeKind::Loop),
        _ => None,
    }
}

fn cpp_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned, String::new()))
        }
        "import.include" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned, String::new()))
        }
        "import.name" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Use, String::new(), name))
        }
        _ => None,
    }
}

/// Extract function signature (parameter list) from the AST.
///
/// The `node` is the `function_declarator` captured by `@definition.function`.
/// It has a `parameters` child field containing the `parameter_list`.
fn cpp_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    if capture_name != "definition.function" {
        return None;
    }
    let params = node.child_by_field_name("parameters")?;
    node_text(params, source)
}


// ── Lexical binding normalize ──────────────────────────────────────────

fn cpp_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn normalize_cpp_lexical(capture_name: &str, node: tree_sitter::Node, source: &str, file_id: FileId) -> Option<BindingDef> {
    let kind = cpp_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);
    let id = BindingId::generate(&file_id, &scope_id, kind.as_str(), &name, range.start_byte);
    Some(BindingDef { id, file_id, function_id: None, scope_id, kind, name, symbol_id: None, range })
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn find_call_expression_cpp(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let kinds: &[&str] = &["call_expression", "new_expression"];
        if kinds.contains(&parent.kind()) { return Some(parent); }
        current = parent;
    }
    None
}

fn normalize_cpp_dataflow_builder(capture_name: &str, node: tree_sitter::Node, source: &str, file_id: FileId) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => node_text(node, source).map(|name| {
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "parameter", Some(&name), Some(&name), range.start_byte);
            (Some(DataNode::parameter(node_id, file_id, None, None, &name, range)), None)
        }).unwrap_or((None, None)),
        "df.assign_target" => node_text(node, source).map(|name| {
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "local", Some(&name), Some(&name), range.start_byte);
            (Some(DataNode::local(node_id, file_id, None, None, &name, range)), None)
        }).unwrap_or((None, None)),
        "df.assign_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_cpp(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "expr", Some(&text), None, range.start_byte);
            (Some(DataNode { id: node_id, file_id, function_id: None, kind: DataNodeKind::Expr, binding_id: None, callsite_id, name: Some(text), access_path: None, arg_index: None, range }), None)
        },
        "df.return_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "return", Some(&text), None, range.start_byte);
            (Some(DataNode { id: node_id, file_id, function_id: None, kind: DataNodeKind::Return, binding_id: None, callsite_id: None, name: Some(text), access_path: None, arg_index: None, range }), None)
        },
        "df.call_target" => node_text(node, source).map(|name| {
            let access_path = name.clone();
            let callsite_id = find_call_expression_cpp(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_target", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::call_target(node_id, file_id, None, callsite_id, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_cpp(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_arg", Some(&text), None, range.start_byte);
            (Some(DataNode::call_arg(node_id, file_id, None, callsite_id, Some(&text), range)), None)
        },
        "df.field_name" => node_text(node, source).map(|name| {
            let access_path = node.parent().filter(|p| p.kind() == "field_expression").and_then(|p| node_text(p, source)).unwrap_or_else(|| name.clone());
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "field", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::field(node_id, file_id, None, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.receiver" | "df.literal" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, if capture_name == "df.literal" { "literal" } else { "receiver" }, Some(&text), None, range.start_byte);
            (Some(DataNode { id: node_id, file_id, function_id: None, kind: if capture_name == "df.literal" { DataNodeKind::Literal } else { DataNodeKind::Receiver }, binding_id: None, callsite_id: None, name: Some(text), access_path: None, arg_index: None, range }), None)
        },
        "df.identifier_use" => {
            if crate::languages::shared::is_identifier_decl_or_property(node, &["template_declaration", "type_definition"]) {
                return (None, None);
            }
            let text = node_text(node, source).unwrap_or_default();
            if text.is_empty() {
                return (None, None);
            }
            let node_id = DataNodeId::generate(
                &file_id, None::<&SymbolId>, "identifier_use",
                Some(&text), Some(&text), range.start_byte,
            );
            let dn = DataNode {
                id: node_id, file_id, function_id: None,
                kind: DataNodeKind::VariableUse,
                binding_id: None, callsite_id: None,
                name: Some(text.clone()), access_path: Some(text),
                arg_index: None, range,
            };
            (Some(dn), None)
        }
        "df.assign_field_target" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id, None::<&SymbolId>, "field",
                Some(&text), Some(&text), range.start_byte,
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
        let spec = CppAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = CppAdapter;
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
        let spec = CppAdapter;
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
        let spec = CppAdapter;
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
        let spec = CppAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = CppAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(query.is_ok(), "dataflow builder query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_reference_and_new_expression() {
        let frontend = super::cpp_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "void f() { int x = 0; int& ref = x; auto p = new Foo(1, 2); }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.cpp");
        let ctx = NormalizeCtx {
            language: Language::Cpp,
            file_id,
            file_path: std::path::Path::new("Test.cpp"),
            source,
        };

        let mut node_hits = 0;
        let mut has_local = false;
        let mut has_call_target = false;
        let mut has_call_arg = false;
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use streaming_iterator::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            let (dn, _de) = frontend.dataflow.normalize(ctx, Capture { name, node: cap.node });
            if let Some(dn) = dn {
                node_hits += 1;
                match dn.kind {
                    DataNodeKind::Local => {
                        // Check for the reference binding "ref"
                        if dn.name.as_deref() == Some("ref") {
                            has_local = true;
                        }
                    }
                    DataNodeKind::CallTarget => {
                        if dn.name.as_deref() == Some("Foo") {
                            has_call_target = true;
                        }
                    }
                    DataNodeKind::CallArg => has_call_arg = true,
                    _ => {}
                }
            }
        }
        assert!(node_hits > 0, "dataflow query should produce DataNodes for ref binding + new expression");
        assert!(has_local, "should have a local DataNode from int& ref = x");
        assert!(has_call_target, "should have a CallTarget DataNode from new Foo(1, 2)");
        assert!(has_call_arg, "should have CallArg DataNodes from new Foo(1, 2)");
    }
}
