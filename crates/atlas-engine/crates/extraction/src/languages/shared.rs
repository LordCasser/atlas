//! Shared helpers for language frontends.
//!
//! ## SymbolDefBuilder
//! Eliminates repeated construction code across frontends by standardizing the
//! `SymbolDef` construction pattern. Every frontend's definition normalizer
//! follows the same flow: (1) determine kind, (2) compute qualified name,
//! (3) optionally extract signature/exported/name_range, (4) build.
//!
//! The builder handles step (4) — SymbolId generation and default field
//! population — so frontends only express what varies.
//!
//! ## make_binding_def
//! Reduces boilerplate in language frontends' dataflow normalize functions.
//! All frontends construct `BindingDef` with the same pattern: generate a
//! deterministic `ScopeId` and `BindingId`, then fill in default fields
//! (`function_id: None`, `symbol_id: None`). The helper centralizes this
//! so frontends only need to provide `file_id`, `kind`, `name`, and `range`.
//!
//! ## Dataflow dispatch helpers
//! Several dataflow dispatch arms are identical across all (or nearly all)
//! language frontends. The `make_df_*` functions extract these patterns so
//! frontends only need a single call per arm. Each helper returns
//! `(Option<DataNode>, Option<DataFlowEdge>)` matching the dataflow
//! normalize function signature.
//!
//! - `make_df_parameter`: shared `"df.parameter"` normalization.
//! - `make_df_assign_target`: shared `"df.assign_target"` normalization
//!   (Ruby has language-specific dispatch on node kind).
//! - `make_df_return_value`: shared `"df.return_value"` normalization
//!   (Python uses `DataNode::return_()`, Cangjie has extra
//!   callsite_id logic). Both are skipped.
//! - `make_df_assign_field_target`: shared `"df.assign_field_target"` arm
//!   (Cangjie and Rust lack this arm;
//!   TypeScript is functionally identical with `name == text`).

// Cargo can compile any subset of frontends. Helpers used only by a disabled
// frontend are intentionally dormant in that build.
#![allow(dead_code)]

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

// ── Shared range helpers ───────────────────────────────────────────────

/// Check if `outer` fully contains `inner` by byte range.
pub(crate) fn contains_range(outer: TextRange, inner: TextRange) -> bool {
    outer.start_byte <= inner.start_byte && outer.end_byte >= inner.end_byte
}

/// Find the innermost scope that fully contains the given byte range.
pub(crate) fn innermost_scope(scopes: &[ScopeDef], range: TextRange) -> Option<ScopeId> {
    scopes
        .iter()
        .filter(|scope| contains_range(scope.range, range))
        .min_by_key(|scope| scope.range.byte_len())
        .map(|scope| scope.id)
}

/// Find the innermost callable symbol containing a source position.
///
/// Function ranges are expanded to their defining scopes before lexical and
/// dataflow extraction, so the same range rule can associate both bindings
/// and data nodes with their owning function. Point facts (where
/// `start_byte == end_byte`) are intentionally supported.
pub(crate) fn innermost_callable_at(symbols: &[SymbolDef], byte: u32) -> Option<SymbolId> {
    symbols
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            ) && symbol.range.start_byte <= byte
                && byte <= symbol.range.end_byte
        })
        .min_by_key(|symbol| symbol.range.byte_len())
        .map(|symbol| symbol.id)
}

/// Select the most recently activated same-named binding at a source byte.
///
/// Multiple declarations may share one lexical scope. Lookup therefore cannot
/// stop at the first name match: it must ignore definitions that are not yet
/// visible and prefer the latest active definition.
pub(crate) fn latest_visible_binding<'a>(
    bindings: impl IntoIterator<Item = &'a BindingDef>,
    name: &str,
    at_byte: u32,
) -> Option<&'a BindingDef> {
    bindings
        .into_iter()
        .filter(|binding| binding.name == name && binding.visible_from_byte <= at_byte)
        .max_by_key(|binding| (binding.visible_from_byte, binding.range.start_byte))
}

// ── Shared binding helpers ──────────────────────────────────────────────

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
        visible_from_byte: range.start_byte,
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
            signature: None,
            exported: false,
        }
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
            name_range: self.range,
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

/// Normalize a language-level signature for compact UI/API display.
///
/// Signatures are persisted facts, so adapters should normalize multiline or
/// oddly spaced syntax into a deterministic single-line form before storing it.
pub fn compact_signature(text: &str) -> Option<String> {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        None
    } else {
        Some(compact)
    }
}

// ── Shared C/C++ declaration helpers ───────────────────────────────────

/// Walk up from a node to find the enclosing function/declaration header.
///
/// Returns the declaration text up to (but not including) the brace or semicolon.
/// Used by C and C++ adapters to extract function signatures.
pub fn find_c_like_declaration_header(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = Some(node);
    while let Some(n) = current {
        match n.kind() {
            "function_definition" | "declaration" | "field_declaration" => {
                let text = super::node_text(n, source)?;
                let header = text
                    .split_once('{')
                    .map(|(head, _)| head)
                    .unwrap_or(text.as_str())
                    .trim()
                    .trim_end_matches(';')
                    .trim();
                return Some(header.to_string());
            }
            _ => current = n.parent(),
        }
    }
    None
}

/// Extract the leading parenthesized group from the start of `text`.
///
/// Returns the substring from the opening `(` through the matching `)`.
/// Used by C and C++ adapters to parse parameter lists from function headers.
pub fn leading_parenthesized(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.first().copied()? != b'(' {
        return None;
    }
    let mut depth = 0u32;
    for (idx, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&text[..=idx]);
                }
            }
            _ => {}
        }
    }
    None
}

// ── Shared dataflow dispatch helpers ────────────────────────────────────

/// Construct a parameter DataNode and return it as `(Some(dn), None)`.
///
/// Used by language frontends for the `"df.parameter"` dataflow
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
/// Used by most language frontends (Ruby is excluded because it has
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
/// Used by most language frontends. **Not used** by:
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

/// Build a DataNode for `df.receiver` or `df.literal` captures.
///
/// Creates a `Receiver` or `Literal` DataNode (no callsite, no access_path).
/// Used by 9 language adapters where the `"df.receiver" | "df.literal"` arm
/// is identical. TypeScript and Python are excluded because they split the
/// two capture names into separate arms; Cangjie is excluded because it
/// merges receiver with field_name.
pub fn make_df_receiver_or_literal(
    file_id: FileId,
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    range: TextRange,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    let text = super::node_text(node, source).unwrap_or_default();
    let node_id = DataNodeId::generate(
        &file_id,
        None::<&SymbolId>,
        if capture_name == "df.literal" {
            "literal"
        } else {
            "receiver"
        },
        Some(&text),
        None,
        range.start_byte,
    );
    let dn = DataNode {
        id: node_id,
        file_id,
        function_id: None,
        kind: if capture_name == "df.literal" {
            DataNodeKind::Literal
        } else {
            DataNodeKind::Receiver
        },
        binding_id: None,
        callsite_id: None,
        name: Some(text),
        access_path: None,
        arg_index: None,
        range,
    };
    (Some(dn), None)
}
/// Construct an Expr DataNode for `"df.assign_value"` and return it as
/// `(Some(dn), None)`.
///
/// Walks up the parent chain to find the enclosing call expression using
/// `find_call_expression` with per-language `call_kinds`. The resulting
/// callsite_id is set on the DataNode for call-graph linking.
///
/// Shared by language frontends for the `"df.assign_value"` arm.
/// Cangjie is excluded because it uses a custom `find_call_expression_cangjie`
/// helper.
pub fn make_df_assign_value(
    file_id: FileId,
    node: tree_sitter::Node,
    source: &str,
    range: TextRange,
    call_kinds: &[&str],
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    let text = super::node_text(node, source).unwrap_or_default();
    let callsite_id = find_call_expression(node, call_kinds)
        .map(|ce| CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
            callsite_id,
            name: Some(text),
            access_path: None,
            arg_index: None,
            range,
        }),
        None,
    )
}

/// Construct a CallArg DataNode for `"df.call_arg"` and return it as
/// `(Some(dn), None)`.
///
/// Walks up the parent chain to find the enclosing call expression using
/// `find_call_expression` with per-language `call_kinds`. The resulting
/// callsite_id is set on the DataNode for call-graph linking.
///
/// Shared by language frontends for the `"df.call_arg"` arm.
/// PHP is excluded because it strips sigils from the text.
/// Cangjie is excluded because it uses a custom `find_call_expression_cangjie`
/// helper.
pub fn make_df_call_arg(
    file_id: FileId,
    node: tree_sitter::Node,
    source: &str,
    range: TextRange,
    call_kinds: &[&str],
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    let text = super::node_text(node, source).unwrap_or_default();
    let callsite_id = find_call_expression(node, call_kinds)
        .map(|ce| CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
    let node_id = DataNodeId::generate(
        &file_id,
        None::<&SymbolId>,
        "call_arg",
        Some(&text),
        None,
        range.start_byte,
    );
    (
        Some(DataNode::call_arg(
            node_id,
            file_id,
            None,
            callsite_id,
            Some(&text),
            range,
        )),
        None,
    )
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
