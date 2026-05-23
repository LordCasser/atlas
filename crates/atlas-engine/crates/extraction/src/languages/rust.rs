//! Rust frontend spec (slot-based).
//!
//! Provides query-driven extraction for Rust source files.
//! Supports: function, method, struct, enum, enum_variant, trait, module, variable,
//! constant, type_alias, macro, field definitions; function calls, macro invocations,
//! field access, type references; use/extern_crate imports; scopes.

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
        FeatureSupport::supported_with_limitations(0.55, vec!["name-based binding (no proper shadowing)"])
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_rust_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for RustAdapter {
    fn dataflow_builder_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported("Rust does not support dataflow extraction")
    }
    fn normalize(
        &self,
        _ctx: NormalizeCtx<'_>,
        _capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        (None, None)
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
            let name = text
                .rsplit("::")
                .next()
                .unwrap_or(&text)
                .to_string();
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
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn normalize_rust_lexical(capture_name: &str, node: tree_sitter::Node, source: &str, file_id: FileId) -> Option<BindingDef> {
    let kind = rust_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);
    let id = BindingId::generate(&file_id, &scope_id, kind.as_str(), &name, range.start_byte);
    Some(BindingDef { id, file_id, function_id: None, scope_id, kind, name, symbol_id: None, range })
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn find_call_expression_rust(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "call_expression" { return Some(parent); }
        current = parent;
    }
    None
}

fn normalize_rust_dataflow_builder(capture_name: &str, node: tree_sitter::Node, source: &str, file_id: FileId) -> (Option<DataNode>, Option<DataFlowEdge>) {
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
            let callsite_id = find_call_expression_rust(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
            let callsite_id = find_call_expression_rust(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_target", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::call_target(node_id, file_id, None, callsite_id, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_rust(node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
            // Skip identifiers that are parameter names or declaration names
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
}
