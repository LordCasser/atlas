//! Shared helpers for language adapters.
//!
//! ## SymbolDefBuilder
//! Eliminates ~60% code duplication across adapters by standardizing the
//! `SymbolDef` construction pattern. Every adapter's `normalize_definition`
//! follows the same flow: (1) determine kind, (2) compute qualified name,
//! (3) optionally extract signature/exported/name_range, (4) build.
//!
//! The builder handles step (4) — SymbolId generation and default field
//! population — so adapters only express what varies.

use types::*;

/// Builder for `SymbolDef` — standardizes the repetitive construction
/// pattern shared by all language adapters.
#[derive(Debug, Clone)]
pub struct SymbolDefBuilder {
    file_id: FileId,
    language: Language,
    kind: SymbolKind,
    name: String,
    qualified_name: String,
    range: TextRange,
    name_range: Option<TextRange>,
    signature: Option<String>,
    exported: bool,
}

impl SymbolDefBuilder {
    /// Create a new builder with required fields.
    pub fn new(
        file_id: FileId,
        language: Language,
        kind: SymbolKind,
        name: String,
        qualified_name: String,
        range: TextRange,
    ) -> Self {
        Self {
            file_id,
            language,
            kind,
            name,
            qualified_name,
            range,
            name_range: None,
            signature: None,
            exported: false,
        }
    }

    /// Set the name-only range (for precise go-to-definition).
    /// If not set, falls back to `range`.
    #[allow(dead_code)]
    pub fn name_range(mut self, r: TextRange) -> Self {
        self.name_range = Some(r);
        self
    }

    /// Set the function/method signature string.
    pub fn signature(mut self, sig: Option<String>) -> Self {
        self.signature = sig;
        self
    }

    /// Set whether the symbol is exported.
    pub fn exported(mut self, exported: bool) -> Self {
        self.exported = exported;
        self
    }

    /// Build the `SymbolDef`.
    ///
    /// Generates a deterministic `SymbolId` from (file_id, language,
    /// qualified_name, kind) via blake3.
    pub fn build(self) -> SymbolDef {
        let symbol_id = SymbolId::generate(
            &self.file_id,
            self.language.as_str(),
            &self.qualified_name,
            self.kind.as_str(),
            None::<&str>,
        );

        SymbolDef {
            id: symbol_id,
            kind: self.kind,
            name: self.name,
            qualified_name: self.qualified_name,
            symbol_path: Vec::new(),
            file_id: self.file_id,
            language: self.language,
            range: self.range,
            name_range: self.name_range.unwrap_or(self.range),
            signature: self.signature,
            visibility: None,
            exported: self.exported,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: Vec::new(),
        }
    }
}

/// Extract the precise range for a name token within a definition node.
///
/// Most adapters currently set `name_range = node_range(node)` which makes
/// go-to-definition highlight the entire declaration. This helper extracts
/// just the name token's range by finding the first child that matches the
/// symbol name text.
#[allow(dead_code)]
pub fn node_name_range(node: tree_sitter::Node, name: &str, source: &str) -> Option<TextRange> {
    // For simple cases where the name is the first identifier child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        let kind = child.kind();
        // Handle common identifier-like node types
        if matches!(
            kind,
            "identifier"
                | "type_identifier"
                | "property_identifier"
                | "shorthand_property_identifier"
        ) {
            if let Ok(text) = child.utf8_text(source.as_bytes()) {
                if text == name {
                    return Some(super::node_range(child));
                }
            }
        }
    }
    // Fall back to the first identifier child
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        if child.kind().contains("identifier") {
            return Some(super::node_range(child));
        }
    }
    None
}

// ── Shared identifier-use filter ────────────────────────────────────────

/// Check if an identifier node is a declaration name or property name
/// (should be excluded from VariableUse generation).
///
/// `extra_decl_kinds`: language-specific declaration node kinds to also check.
/// Common kinds (variable_declarator, function_declaration, class_declaration,
/// parameter, catch_clause) are always checked.
pub(crate) fn is_identifier_decl_or_property(
    node: tree_sitter::Node,
    extra_decl_kinds: &[&str],
) -> bool {
    let parent = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    let parent_kind = parent.kind();

    // Common declaration kinds across all languages
    let is_common_decl = matches!(
        parent_kind,
        "variable_declarator" | "function_declaration" | "class_declaration"
        | "method_declaration" | "method_definition" | "constructor_declaration"
        | "interface_declaration" | "enum_declaration" | "struct_specifier"
        | "function_definition" | "function_item" | "property_declaration"
        | "required_parameter" | "optional_parameter" | "formal_parameter"
        | "parameter" | "simple_parameter" | "parameter_declaration"
        | "catch_clause" | "catch_declaration" | "catch_formal_parameter"
        | "import_specifier" | "import_clause" | "namespace_import"
        | "aliased_import" | "import_statement" | "for_statement"
        | "foreach_statement" | "enhanced_for_statement" | "with_item"
        | "except_clause" | "static_variable_declaration"
        | "field_declaration" | "public_field_definition"
    ) || extra_decl_kinds.contains(&parent_kind);

    if is_common_decl {
        // Check if this node is the "name" field of its parent
        if parent.child_by_field_name("name").map_or(false, |n| n.id() == node.id()) {
            return true;
        }
    }

    // Property names in member/field access expressions
    let is_property = matches!(
        parent_kind,
        "member_expression" | "field_expression" | "field_access"
        | "selector_expression" | "navigation_expression"
        | "member_access_expression" | "attribute"
    );
    if is_property {
        for field in &["property", "field", "attribute"] {
            if parent.child_by_field_name(field).map_or(false, |n| n.id() == node.id()) {
                return true;
            }
        }
    }

    // Type annotations / type arguments
    if matches!(parent_kind, "type_annotation" | "type_arguments" | "type_parameters" | "generic_type") {
        return true;
    }

    false
}

// ── Shared dataflow normalize ───────────────────────────────────────────
// ... existing shared functions follow ...

use types::*;

/// Walk up the AST to find the enclosing call expression.
pub(crate) fn find_call_expr<'a>(call_kind: &str, node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == call_kind { return Some(parent); }
        current = parent;
    }
    None
}

/// Shared dataflow normalize for all common capture types.
pub(crate) fn normalize_dataflow(
    call_kind: &str,
    field_kind: &str,
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = super::node_range(node);
    match capture_name {
        "df.parameter" => super::node_text(node, source).map(|name| {
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "parameter", Some(&name), Some(&name), range.start_byte);
            (Some(DataNode::parameter(nid, file_id, None, None, &name, range)), None)
        }).unwrap_or((None, None)),
        "df.assign_target" => super::node_text(node, source).map(|name| {
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "local", Some(&name), Some(&name), range.start_byte);
            (Some(DataNode::local(nid, file_id, None, None, &name, range)), None)
        }).unwrap_or((None, None)),
        "df.assign_value" => {
            let text = super::node_text(node, source).unwrap_or_default();
            let csid = find_call_expr(call_kind, node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "expr", Some(&text), None, range.start_byte);
            (Some(DataNode { id: nid, file_id, function_id: None, kind: DataNodeKind::Expr, binding_id: None, callsite_id: csid, name: Some(text), access_path: None, arg_index: None, range }), None)
        }
        "df.return_value" => {
            let text = super::node_text(node, source).unwrap_or_default();
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "return", Some(&text), None, range.start_byte);
            (Some(DataNode { id: nid, file_id, function_id: None, kind: DataNodeKind::Return, binding_id: None, callsite_id: None, name: Some(text), access_path: None, arg_index: None, range }), None)
        }
        "df.call_target" => super::node_text(node, source).map(|name| {
            let access_path = name.clone();
            let csid = find_call_expr(call_kind, node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_target", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::call_target(nid, file_id, None, csid, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.call_arg" => {
            let text = super::node_text(node, source).unwrap_or_default();
            let csid = find_call_expr(call_kind, node).map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "call_arg", Some(&text), None, range.start_byte);
            (Some(DataNode::call_arg(nid, file_id, None, csid, Some(&text), range)), None)
        }
        "df.field_name" => super::node_text(node, source).map(|name| {
            let access_path = node.parent().filter(|p| p.kind() == field_kind).and_then(|p| super::node_text(p, source)).unwrap_or_else(|| name.clone());
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "field", Some(&name), Some(&access_path), range.start_byte);
            (Some(DataNode::field(nid, file_id, None, &name, &access_path, range)), None)
        }).unwrap_or((None, None)),
        "df.assign_field_target" => {
            let text = super::node_text(node, source).unwrap_or_default();
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "field", Some(&text), Some(&text), range.start_byte);
            (Some(DataNode::field(nid, file_id, None, &text, &text, range)), None)
        }
        "df.receiver" | "df.literal" => {
            let text = super::node_text(node, source).unwrap_or_default();
            let is_lit = capture_name == "df.literal";
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, if is_lit { "literal" } else { "receiver" }, Some(&text), None, range.start_byte);
            (Some(DataNode { id: nid, file_id, function_id: None, kind: if is_lit { DataNodeKind::Literal } else { DataNodeKind::Receiver }, binding_id: None, callsite_id: None, name: Some(text), access_path: None, arg_index: None, range }), None)
        }
        "df.identifier_use" => {
            let text = super::node_text(node, source).unwrap_or_default();
            if text.is_empty() { return (None, None); }
            let nid = DataNodeId::generate(&file_id, None::<&SymbolId>, "identifier_use", Some(&text), Some(&text), range.start_byte);
            (Some(DataNode { id: nid, file_id, function_id: None, kind: DataNodeKind::VariableUse, binding_id: None, callsite_id: None, name: Some(text.clone()), access_path: Some(text), arg_index: None, range }), None)
        }
        _ => (None, None),
    }
}
