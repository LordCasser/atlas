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
//!
//! ## make_binding_def
//! Reduces boilerplate in language adapters' dataflow normalize functions.
//! All adapters construct `BindingDef` with the same pattern: generate a
//! deterministic `ScopeId` and `BindingId`, then fill in default fields
//! (`function_id: None`, `symbol_id: None`). The helper centralizes this
//! so adapters only need to provide `file_id`, `kind`, `name`, and `range`.
//!
//! ## Dataflow dispatch helpers
//! Several dataflow dispatch arms are identical across all (or nearly all)
//! language adapters. The `make_df_*` functions extract these patterns so
//! adapters only need a single call per arm. Each helper returns
//! `(Option<DataNode>, Option<DataFlowEdge>)` matching the dataflow
//! normalize function signature.
//!
//! - `make_df_parameter`: `"df.parameter"` arm — identical in all 12 adapters.
//! - `make_df_assign_target`: `"df.assign_target"` arm — identical in 11/12
//!   adapters (Ruby has language-specific dispatch on node kind).
//! - `make_df_return_value`: `"df.return_value"` arm — identical in 11/12
//!   adapters (Python uses `DataNode::return_()`, Cangjie has extra
//!   callsite_id logic). Both are skipped.
//! - `make_df_assign_field_target`: `"df.assign_field_target"` arm — identical
//!   in 10 adapters that have this arm (Cangjie and Rust lack this arm;
//!   TypeScript is functionally identical with `name == text`).

use types::*;

/// Construct a `ScopeDef` with ID generation and default optional fields, reducing
/// boilerplate in language adapters' scope normalize functions.
///
/// Parameters:
/// - `file_id`: The file's `FileId`
/// - `kind`: The `ScopeKind` determined by the per-language mapping
/// - `name`: The scope name (language-specific)
/// - `scope_path`: The scope path (often same as name, or empty for C/C++)
/// - `range`: The node's `TextRange`
pub fn make_scope_def(
    file_id: FileId,
    kind: ScopeKind,
    name: String,
    scope_path: String,
    range: TextRange,
) -> ScopeDef {
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);
    ScopeDef {
        id: scope_id,
        file_id,
        kind,
        name,
        scope_path,
        parent_id: None,
        range,
    }
}

/// Construct a `ScopeDef` with auto-generated name (`"{kind}#{start_byte}"`)
/// and scope_path mirroring the name. Used by most language adapters.
pub fn make_scope_def_auto_name(file_id: FileId, kind: ScopeKind, range: TextRange) -> ScopeDef {
    let name = format!("{:?}#{}", kind, range.start_byte);
    make_scope_def(file_id, kind, name.clone(), name, range)
}

/// Construct a `BindingDef` with default fields, reducing boilerplate in
/// language adapters' dataflow normalize functions.
///
/// Generates deterministic `ScopeId` and `BindingId` from the provided
/// fields, and sets `function_id` and `symbol_id` to `None`.
pub fn make_binding_def(
    file_id: FileId,
    kind: BindingKind,
    name: String,
    range: TextRange,
) -> BindingDef {
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);
    let id = BindingId::generate(&file_id, &scope_id, kind.as_str(), &name, range.start_byte);
    BindingDef {
        id,
        file_id,
        function_id: None,
        scope_id,
        kind,
        name,
        symbol_id: None,
        range,
    }
}

/// Construct a `ReferenceUse` with default optional fields, reducing boilerplate
/// in language adapters' reference normalize functions.
///
/// Parameters:
/// - `file_id`: The file's `FileId`
/// - `kind`: The `ReferenceKind` determined by the per-language mapping
/// - `text`: The reference text (usually from node_text)
/// - `name`: The reference name (may differ from text for qualified references)
/// - `range`: The node's `TextRange`
pub fn make_reference_use(
    file_id: FileId,
    kind: ReferenceKind,
    text: String,
    name: String,
    range: TextRange,
) -> ReferenceUse {
    let ref_id = ReferenceId::generate(
        &file_id,
        None::<&SymbolId>,
        range.start_byte,
        range.end_byte,
        &text,
        kind,
    );
    ReferenceUse {
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
    }
}

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
            layer: "structural".to_string(),
        }
    }
}

// ── Shared dataflow dispatch helpers ────────────────────────────────────

/// Construct a parameter DataNode and return it as `(Some(dn), None)`.
///
/// Used by all 12 language adapters for the `"df.parameter"` dataflow
/// dispatch arm. Extracts the node text as the parameter name.
pub fn make_df_parameter(
    file_id: FileId,
    node: tree_sitter::Node,
    source: &str,
    range: TextRange,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    super::node_text(node, source)
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
        .unwrap_or((None, None))
}

/// Construct a local-variable DataNode for `"df.assign_target"` and return
/// it as `(Some(dn), None)`.
///
/// Used by 11/12 language adapters (Ruby is excluded because it has
/// language-specific dispatch on node kind for instance/class/global variables).
pub fn make_df_assign_target(
    file_id: FileId,
    node: tree_sitter::Node,
    source: &str,
    range: TextRange,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    super::node_text(node, source)
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
        .unwrap_or((None, None))
}

/// Construct a Return DataNode for `"df.return_value"` and return it as
/// `(Some(dn), None)`.
///
/// Used by 11/12 language adapters. **Not used** by:
/// - Python: uses `DataNode::return_()` (no text/name, different ID gen).
/// - Cangjie: has extra callsite_id logic and walks to the expression child.
///
/// Those adapters keep their own inline implementations.
pub fn make_df_return_value(
    file_id: FileId,
    node: tree_sitter::Node,
    source: &str,
    range: TextRange,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    let text = super::node_text(node, source).unwrap_or_default();
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
        callsite_id: None,
        name: Some(text),
        access_path: None,
        arg_index: None,
        range,
    };
    (Some(dn), None)
}

/// Construct a Field DataNode for `"df.assign_field_target"` and return it
/// as `(Some(dn), None)`.
///
/// The caller must provide the `access_path` (usually computed from
/// `node_text(node, source).unwrap_or_default()` or a per-language
/// `access_path_for` helper). This helper does **not** compute the
/// access_path itself — that logic stays per-language.
///
/// Used by all 10 adapters that have the `df.assign_field_target` arm
/// (Cangjie and Rust lack this arm entirely; TypeScript is functionally
/// identical because its `name = text.clone()`).
pub fn make_df_assign_field_target(
    file_id: FileId,
    access_path: &str,
    range: TextRange,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    let node_id = DataNodeId::generate(
        &file_id,
        None::<&SymbolId>,
        "field",
        Some(access_path),
        Some(access_path),
        range.start_byte,
    );
    let dn = DataNode::field(node_id, file_id, None, access_path, access_path, range);
    (Some(dn), None)
}

// ── Shared call-expression ancestor walk ───────────────────────────────

/// Walk up the AST parent chain from `node` to find the first ancestor
/// whose kind matches one of `call_kinds`. Checks `node` itself first.
pub fn find_call_expression<'a>(
    node: tree_sitter::Node<'a>,
    call_kinds: &[&str],
) -> Option<tree_sitter::Node<'a>> {
    if call_kinds.contains(&node.kind()) {
        return Some(node);
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        if call_kinds.contains(&parent.kind()) {
            return Some(parent);
        }
        current = parent;
    }
    None
}

// ── Shared identifier-use filter ────────────────────────────────────────

/// Check if an identifier node sits inside an assignment left-hand side.
///
/// Walks up from `node` to find an enclosing assignment-like parent
/// and checks whether the node is contained within the `left` field.
/// This prevents `x` in `x = y` from being captured as a VariableUse
/// (it is already captured separately as `df.assign_target`).
///
/// Uses byte-range containment rather than strict field equality because
/// the left-hand side may be a compound expression (e.g. `obj.field`).
fn is_inside_assignment_left(node: tree_sitter::Node) -> bool {
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        let pk = parent.kind();
        let is_assignment_like = pk.contains("assignment")
            || pk == "assignment_expression"
            || pk == "assignment_statement"
            || pk == "assignment";
        if is_assignment_like {
            if let Some(left) = parent.child_by_field_name("left") {
                return left.start_byte() <= node.start_byte()
                    && left.end_byte() >= node.end_byte();
            }
            return false;
        }
        cur = parent;
    }
    false
}

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

    // Exclude identifiers that are assignment left-hand side targets
    // (already captured as df.assign_target / assign_field_target).
    // Handles compound LHS like `obj.field` via byte-range containment.
    if is_inside_assignment_left(node) {
        return true;
    }

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
        // C/C++ specific: init_declarator wraps the declarator (identifier)
        // in declarations like `int x = 1;`
        | "init_declarator"
    ) || extra_decl_kinds.contains(&parent_kind);

    if is_common_decl {
        // Check if this node is the "name" or "declarator" field of its parent.
        // C/C++ uses "declarator" for identifiers in init_declarator nodes.
        if parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id())
            || parent
                .child_by_field_name("declarator")
                .is_some_and(|n| n.id() == node.id())
        {
            return true;
        }
    }

    // Property names in member/field access expressions
    let is_property = matches!(
        parent_kind,
        "member_expression"
            | "field_expression"
            | "field_access"
            | "selector_expression"
            | "navigation_expression"
            | "member_access_expression"
            | "attribute"
    );
    if is_property {
        for field in &["property", "field", "attribute"] {
            if parent
                .child_by_field_name(field)
                .is_some_and(|n| n.id() == node.id())
            {
                return true;
            }
        }
    }

    // Type annotations / type arguments
    if matches!(
        parent_kind,
        "type_annotation" | "type_arguments" | "type_parameters" | "generic_type"
    ) {
        return true;
    }

    false
}
