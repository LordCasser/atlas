//! C# frontend spec (slot-based).
//!
//! Provides query-driven extraction for C# source files.
//! Supports: class, struct, interface, enum, enum_member, method, constructor,
//! property, field, variable, namespace, delegate definitions;
//! method calls, field access, type references, instantiation; using directives;
//! scopes.

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

    let ref_id = ReferenceId::generate(
        &file_id,
        None::<&SymbolId>,
        range.start_byte,
        range.end_byte,
        &text,
        kind,
    );

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
            0.70,
            vec!["name-based binding (no proper shadowing)"],
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
            0.70,
            vec!["capture-order assignment pairing (Nth target ≈ Nth expr)"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_csharp_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
                if let Some(type_name) = parent.child_by_field_name("name") {
                    if let Ok(type_str) = type_name.utf8_text(source.as_bytes()) {
                        parts.push(type_str.to_string());
                    }
                }
            }
            "namespace_declaration" => {
                if let Some(ns_name) = parent.child_by_field_name("name") {
                    if let Ok(ns_str) = ns_name.utf8_text(source.as_bytes()) {
                        parts.push(ns_str.to_string());
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
            let params = parent.child_by_field_name("parameter_list")?;
            Some(node_text(params, source)?)
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
        _ => None,
    }
}

fn normalize_csharp_lexical(capture_name: &str, node: tree_sitter::Node, source: &str, file_id: FileId) -> Option<BindingDef> {
    let kind = csharp_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);
    let id = BindingId::generate(&file_id, &scope_id, kind.as_str(), &name, range.start_byte);
    Some(BindingDef { id, file_id, function_id: None, scope_id, kind, name, symbol_id: None, range })
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn find_call_expression_csharp(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "invocation_expression" { return Some(parent); }
        current = parent;
    }
    None
}

fn normalize_csharp_dataflow_builder(capture_name: &str, node: tree_sitter::Node, source: &str, file_id: FileId) -> (Option<DataNode>, Option<DataFlowEdge>) {
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
            let callsite_id = find_call_expression_csharp(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "expr", Some(&text), None, range.start_byte);
            (Some(DataNode { id: node_id, file_id, function_id: None, kind: DataNodeKind::Expr, binding_id: None, callsite_id, name: Some(text), access_path: None, arg_index: None, range }), None)
        }
        "df.return_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "return", Some(&text), None, range.start_byte);
            (Some(DataNode { id: node_id, file_id, function_id: None, kind: DataNodeKind::Return, binding_id: None, callsite_id: None, name: Some(text), access_path: None, arg_index: None, range }), None)
        }
        "df.call_target" => node_text(node, source).map(|name| {
            let access_path = name.clone();
            let callsite_id = find_call_expression_csharp(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_target", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::call_target(node_id, file_id, None, callsite_id, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_csharp(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_arg", Some(&text), None, range.start_byte);
            (Some(DataNode::call_arg(node_id, file_id, None, callsite_id, Some(&text), range)), None)
        }
        "df.field_name" => node_text(node, source).map(|name| {
            let access_path = node.parent().filter(|p| p.kind() == "member_access_expression").and_then(|p| node_text(p, source)).unwrap_or_else(|| name.clone());
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "field", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::field(node_id, file_id, None, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.receiver" | "df.literal" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, if capture_name == "df.literal" { "literal" } else { "receiver" }, Some(&text), None, range.start_byte);
            (Some(DataNode { id: node_id, file_id, function_id: None, kind: if capture_name == "df.literal" { DataNodeKind::Literal } else { DataNodeKind::Receiver }, binding_id: None, callsite_id: None, name: Some(text), access_path: None, arg_index: None, range }), None)
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
}
