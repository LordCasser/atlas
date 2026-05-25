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
        if parent.child_by_field_name("name").map_or(false, |n| n.id() == node.id())
            || parent.child_by_field_name("declarator").map_or(false, |n| n.id() == node.id())
        {
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

