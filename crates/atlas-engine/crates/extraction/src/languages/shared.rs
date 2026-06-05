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

use types::*;

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
    let ref_id = ReferenceId::generate(&file_id, None::<&SymbolId>, range.start_byte, range.end_byte, &text, kind);
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
            layer: "structural".to_string(),
        }
    }
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
#[allow(dead_code)] // used by language adapters gated behind features
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
