//! TypeScript / JavaScript frontend spec (slot-based).
//!
//! Uses tree-sitter-typescript grammar and embedded query files.
//! JavaScript is treated as a subset of TypeScript for extraction purposes.
//!
//! `TypeScriptFrontendSpec` implements every slot trait (ParserSpec through
//! DataflowSpec) via shared private normalize helpers.

use crate::languages::shared::{
    compact_signature, make_binding_def, make_df_assign_field_target, make_df_assign_target,
    make_df_assign_value, make_df_call_arg, make_df_parameter, make_df_return_value,
    make_reference_use, make_scope_def_auto_name,
};
use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, ImportExtractorSpec, LanguageFrontend, LexicalBindingSpec, NormalizeCtx,
    ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec, SymbolExtractorSpec,
};

use types::*;

// ---------------------------------------------------------------------------
// Spec struct
// ---------------------------------------------------------------------------

/// TypeScript frontend spec that implements every slot trait (ParserSpec
/// through DataflowSpec).
pub(crate) struct TypeScriptFrontendSpec;

// ---------------------------------------------------------------------------
// Slot trait implementations — each calls the private normalize_ts_* helpers.
// ---------------------------------------------------------------------------

impl ParserSpec for TypeScriptFrontendSpec {
    fn language(&self) -> Language {
        Language::TypeScript
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

impl SymbolExtractorSpec for TypeScriptFrontendSpec {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/typescript/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/typescript/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_ts_definition(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.language,
        )
    }
}

impl ReferenceExtractorSpec for TypeScriptFrontendSpec {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/typescript/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_ts_reference(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl ImportExtractorSpec for TypeScriptFrontendSpec {
    fn import_query(&self) -> &str {
        include_str!("../../queries/typescript/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_ts_import(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl ScopeExtractorSpec for TypeScriptFrontendSpec {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/typescript/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_ts_scope(&capture.name, capture.node, ctx.file_id)
    }
}

impl LexicalBindingSpec for TypeScriptFrontendSpec {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/typescript/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_ts_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for TypeScriptFrontendSpec {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/typescript/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["AST-driven local dataflow with language-specific gaps"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_ts_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

pub(crate) fn normalize_ts_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    language: Language,
) -> Option<SymbolDef> {
    use super::shared::SymbolDefBuilder;

    let kind = ts_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let name_range = node_range(node);
    let range = ts_declaration_range(capture_name, node).unwrap_or(name_range);

    let qualified_name = qualified_name_from_node("", &name, node, source);
    let exported = is_exported_in_tree(node);
    let async_ = is_async_definition(node);
    let signature = ts_extract_signature(capture_name, node, source).map(|signature| {
        if async_ {
            format!("async {signature}")
        } else {
            signature
        }
    });

    let mut symbol = SymbolDefBuilder::new(file_id, language, kind, name, qualified_name, range)
        .exported(exported)
        .signature(signature)
        .build();
    symbol.name_range = name_range;
    symbol.async_ = async_;
    Some(symbol)
}

pub(crate) fn normalize_ts_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = ts_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

pub(crate) fn normalize_ts_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = ts_import_info(capture_name, node, source)?;
    let range = import_statement_range(node);

    // For aliased imports/exports, the captured node is the alias (e.g. `bar` in
    // `import { foo as bar }`), but imported_name should hold the original exported
    // name. Walk up to the parent specifier's `name` field to get it.
    let (imported_name, local_name) = if capture_name == "import.default" {
        ("default".to_string(), Some(imported_name))
    } else if capture_name == "export.default" {
        (imported_name, Some("default".to_string()))
    } else if capture_name == "import.alias" || capture_name == "export.alias" {
        // parent is import_specifier or export_specifier — both have a `name` field
        let original = node
            .parent()
            .and_then(|p| p.child_by_field_name("name"))
            .and_then(|n| node_text(n, source))
            .unwrap_or_else(|| imported_name.clone());
        let alias = imported_name.clone(); // this is the alias text from ts_import_info
        (original, Some(alias))
    } else {
        (imported_name, None)
    };
    let is_relative = module.starts_with('.');
    let is_wildcard = matches!(capture_name, "import.namespace" | "export.module");

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
        local_name,
        is_wildcard,
        is_relative,
        range,
        alias: None,
    })
}

/// Return the complete source statement represented by an import capture.
fn import_statement_range(mut node: tree_sitter::Node<'_>) -> TextRange {
    loop {
        if matches!(
            node.kind(),
            "import_statement"
                | "export_statement"
                | "lexical_declaration"
                | "variable_declaration"
                | "expression_statement"
        ) {
            return node_range(node);
        }
        let Some(parent) = node.parent() else {
            return node_range(node);
        };
        node = parent;
    }
}

pub(crate) fn normalize_ts_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.function" => ScopeKind::Function,
        "scope.method" => ScopeKind::Method,
        "scope.class" => ScopeKind::Class,
        "scope.interface" => ScopeKind::Interface,
        "scope.enum" => ScopeKind::Enum,
        "scope.namespace" => ScopeKind::Namespace,
        "scope.block" => ScopeKind::Block,
        "scope.conditional" => ScopeKind::Conditional,
        "scope.loop" => ScopeKind::Loop,
        _ => return None,
    };
    let range = node_range(node);

    Some(make_scope_def_auto_name(file_id, kind, range))
}

pub(crate) fn normalize_ts_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = ts_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

/// Check if a tree-sitter identifier node is a declaration name, property name,
/// or type name — i.e., it should NOT be treated as an identifier use.
fn is_ts_identifier_declaration_or_property(node: tree_sitter::Node) -> bool {
    let parent = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    match parent.kind() {
        // Declaration names
        "variable_declarator"
        | "function_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "method_definition"
        | "method_signature"
        | "abstract_method_signature"
        | "interface_declaration"
        | "enum_declaration"
        | "enum_assignment"
        | "type_alias_declaration"
        | "module"
        | "import_specifier"
        | "import_clause"
        | "namespace_import"
        | "catch_clause"
        | "public_field_definition"
        | "property_signature"
        | "required_parameter"
        | "optional_parameter" => {
            // Check if this node is the "name" field of the parent
            parent
                .child_by_field_name("name")
                .is_some_and(|n| n.id() == node.id())
        }
        // Property names in member expressions (obj.property)
        "member_expression" => parent
            .child_by_field_name("property")
            .is_some_and(|n| n.id() == node.id()),
        // Type references (like `string`, `number` in type annotations)
        "type_annotation" | "type_arguments" | "type_parameters" => true,
        _ => false,
    }
}

pub(crate) fn normalize_ts_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;

    let range = node_range(node);

    match capture_name {
        "df.parameter" => make_df_parameter(file_id, node, source, range),
        "df.assign_target" => make_df_assign_target(file_id, node, source, range),
        "df.assign_value" => make_df_assign_value(
            file_id,
            node,
            source,
            range,
            &["call_expression", "new_expression"],
        ),
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_arg" => make_df_call_arg(
            file_id,
            node,
            source,
            range,
            &["call_expression", "new_expression"],
        ),
        "df.call_target" => node_text(node, source)
            .map(|terminal_name| {
                // For member_expression captures (e.g., "conn.close"), walk up to the
                // full member_expression to get the qualified callee text ("conn.close").
                // For plain identifier captures (e.g., "open"), use the terminal text.
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "member_expression")
                    .and_then(|p| node_text(p, source))
                    .unwrap_or_else(|| terminal_name.clone());
                // Use the full qualified text as `name` so Suffix rules (".close")
                // match against "conn.close" instead of just "close".
                let name = access_path.clone();
                let callsite_id = crate::languages::shared::find_call_expression(
                    node,
                    &["call_expression", "new_expression"],
                )
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&types::ids::SymbolId>,
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
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "member_expression")
                    .and_then(|p| node_text(p, source))
                    .unwrap_or_else(|| name.clone());
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&types::ids::SymbolId>,
                    "field",
                    Some(&name),
                    Some(&access_path),
                    range.start_byte,
                );
                let dn = DataNode::field(node_id, file_id, None, &name, &access_path, range);
                (Some(dn), None)
            })
            .unwrap_or((None, None)),
        "df.receiver" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "receiver",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::Receiver,
                binding_id: None,
                callsite_id: None,
                name: Some(text),
                access_path: None,
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.literal" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "literal",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::Literal,
                binding_id: None,
                callsite_id: None,
                name: Some(text),
                access_path: None,
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.await_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = crate::languages::shared::find_call_expression(
                node,
                &["call_expression", "new_expression"],
            )
            .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "expr",
                Some("await expr"),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::Expr,
                binding_id: None,
                callsite_id,
                name: Some("await expr".to_string()),
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
        "df.identifier_use" => {
            // Filter out identifiers that are declaration names, property names,
            // type names, or callee targets (already captured by other patterns).
            if is_ts_identifier_declaration_or_property(node) {
                return (None, None);
            }
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "identifier_use",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::VariableUse,
                binding_id: None,
                callsite_id: None,
                name: Some(text.clone()),
                access_path: Some(text),
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.react_cleanup_return" => {
            // React useEffect cleanup return: `return () => { cleanup(); }`
            let text = node_text(node, source).unwrap_or_else(|| "<cleanup>".to_string());
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "cleanup_return",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::CleanupReturn,
                binding_id: None,
                callsite_id: None,
                name: Some("<cleanup>".to_string()),
                access_path: Some(text),
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        _ => (None, None),
    }
}

/// Construct a [`LanguageFrontend`] directly from TypeScript-specific slot
/// implementations — no adapter wrapper needed.
/// This is the canonical TypeScript frontend factory.
pub fn typescript_frontend() -> LanguageFrontend {
    use crate::callsite_spec::create_extractor;
    use crate::frontend::FrontendParts;
    use types::capability::LanguageCapabilityProfile;

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(TypeScriptFrontendSpec),
        symbols: Box::new(TypeScriptFrontendSpec),
        references: Box::new(TypeScriptFrontendSpec),
        imports: Box::new(TypeScriptFrontendSpec),
        scopes: Box::new(TypeScriptFrontendSpec),
        callsites: create_extractor(Language::TypeScript),
        lexical: Box::new(TypeScriptFrontendSpec),
        dataflow: Box::new(TypeScriptFrontendSpec),
        capability: LanguageCapabilityProfile::for_language(Language::TypeScript),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map lexical capture name to BindingKind.
fn ts_binding_kind(capture_name: &str) -> Option<types::enums::BindingKind> {
    use types::enums::BindingKind;
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.import_alias" => Some(BindingKind::ImportAlias),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        "lexical.field" => Some(BindingKind::Field),
        _ => None,
    }
}

fn ts_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" | "definition.method" => {
            let declaration = node.parent()?;
            let mut signature = String::new();

            if let Some(type_params) = declaration.child_by_field_name("type_parameters") {
                signature.push_str(&node_text(type_params, source)?);
            }

            let params = declaration.child_by_field_name("parameters")?;
            signature.push_str(&node_text(params, source)?);

            if let Some(return_type) = declaration.child_by_field_name("return_type") {
                signature.push_str(&node_text(return_type, source)?);
            }

            compact_signature(&signature)
        }
        "definition.class" => compact_signature(&ts_leading_decorators(node, source).join(" ")),
        "definition.field" | "definition.property" => {
            let declaration = node.parent()?;
            let mut signature = ts_leading_decorators(node, source).join(" ");
            if let Some(type_node) = declaration.child_by_field_name("type") {
                if !signature.is_empty() {
                    signature.push(' ');
                }
                signature.push_str(&node_text(type_node, source)?);
            }
            compact_signature(&signature)
        }
        _ => None,
    }
}

fn ts_leading_decorators(node: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let Some(mut declaration) = node.parent() else {
        return Vec::new();
    };
    if let Some(parent) = declaration.parent()
        && parent.kind() == "export_statement"
    {
        declaration = parent;
    }

    let mut decorators = Vec::new();
    let mut previous = declaration.prev_named_sibling();
    while let Some(decorator) = previous.filter(|candidate| candidate.kind() == "decorator") {
        if let Some(text) = node_text(decorator, source) {
            decorators.push(text);
        }
        previous = decorator.prev_named_sibling();
    }
    decorators.reverse();
    decorators
}

fn ts_declaration_node<'tree>(
    capture_name: &str,
    node: tree_sitter::Node<'tree>,
) -> Option<tree_sitter::Node<'tree>> {
    let mut declaration = node.parent()?;
    if capture_name == "definition.variable"
        && declaration.kind() == "variable_declarator"
        && let Some(parent) = declaration.parent()
        && matches!(
            parent.kind(),
            "lexical_declaration" | "variable_declaration"
        )
    {
        declaration = parent;
    }
    if let Some(parent) = declaration.parent()
        && parent.kind() == "export_statement"
    {
        declaration = parent;
    }
    Some(declaration)
}

fn ts_declaration_range(capture_name: &str, node: tree_sitter::Node<'_>) -> Option<TextRange> {
    let declaration = ts_declaration_node(capture_name, node)?;
    let mut range = node_range(declaration);
    let mut previous = declaration.prev_named_sibling();
    while let Some(decorator) = previous.filter(|node| node.kind() == "decorator") {
        let decorator_range = node_range(decorator);
        range.start_byte = decorator_range.start_byte;
        range.start_line = decorator_range.start_line;
        range.start_column = decorator_range.start_column;
        previous = decorator.prev_named_sibling();
    }
    Some(range)
}

/// Infer a qualified name from node's parent hierarchy.
fn qualified_name_from_node(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    // Walk up parent scopes to build qualified name
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration"
            | "abstract_class_declaration"
            | "class"
            | "interface_declaration"
            | "enum_declaration" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(class_name.to_string());
                    }
                }
            }
            "namespace_declaration" | "module" => {
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
    let prefix_str = if prefix.is_empty() { "" } else { prefix };
    if prefix_str.is_empty() {
        parts.join(".")
    } else {
        format!("{}.{}", prefix_str, parts.join("."))
    }
}

fn is_async_definition(node: tree_sitter::Node<'_>) -> bool {
    let Some(declaration) = node.parent() else {
        return false;
    };
    let mut cursor = declaration.walk();
    declaration
        .children(&mut cursor)
        .any(|child| child.kind() == "async")
}

/// Check whether a TS/JS node is inside an `export` statement.
fn is_exported_in_tree(node: tree_sitter::Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let kind = parent.kind();
        if kind == "export_statement" || kind.contains("export") {
            return true;
        }
        // Stop at the top-level declaration container
        if kind == "program" || kind == "statement_block" {
            break;
        }
        current = parent;
    }
    false
}

/// Map capture name to SymbolKind.
fn ts_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.field" => Some(SymbolKind::Field),
        "definition.property" => Some(SymbolKind::Property),
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.enum_member" => Some(SymbolKind::EnumMember),
        "definition.type_alias" => Some(SymbolKind::TypeAlias),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn ts_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.extends" => Some(ReferenceKind::Inheritance),
        "reference.implements" => Some(ReferenceKind::Implementation),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.usage" => Some(ReferenceKind::Usage),
        _ => None,
    }
}

/// Extract import info from capture.
fn ts_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let module_path = node_text(node, source)?;
            let cleaned = module_path
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
            Some((ImportKind::Import, cleaned, String::new()))
        }
        "import.name" | "import.alias" | "import.default" => {
            let name = node_text(node, source)?;
            // Walk up to the enclosing import_statement to find the module source
            let module = extract_module_from_ancestor(node, source);
            Some((ImportKind::Import, module, name))
        }
        "import.namespace" => {
            let name = node_text(node, source)?;
            let module = extract_module_from_ancestor(node, source);
            Some((ImportKind::Import, module, name))
        }
        // ── CommonJS require ───────────────────────────────────────────
        "import.require_module" => {
            let module_path = node_text(node, source)?;
            let cleaned = module_path
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
            Some((ImportKind::Import, cleaned, String::new()))
        }
        "import.require_name" => {
            let name = node_text(node, source)?;
            let module = extract_require_module_from_variable_declarator(node, source);
            Some((ImportKind::Import, module, name))
        }
        // ── Barrel re-exports ──────────────────────────────────────────
        "export.module" => {
            let module_path = node_text(node, source)?;
            let cleaned = module_path
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
            // Wildcard re-export: `export * from './bar'`
            Some((ImportKind::ExportFrom, cleaned, String::new()))
        }
        "export.name" | "export.alias" => {
            let name = node_text(node, source)?;
            // Walk up to the enclosing export_statement to find the module source
            let module = extract_export_module_from_ancestor(node, source);
            Some((ImportKind::ExportFrom, module, name))
        }
        "export.default" => {
            let name = node_text(node, source)?;
            Some((ImportKind::ExportFrom, String::new(), name))
        }
        // ── CommonJS exports ────────────────────────────────────────────
        // module.exports = x  → default export, self-referential (no module)
        "export.cjs_default" => {
            let name = node_text(node, source)?;
            Some((ImportKind::ExportFrom, String::new(), name))
        }
        // exports.foo = x  → named export, self-referential
        "export.cjs_name" => {
            let name = node_text(node, source)?;
            Some((ImportKind::ExportFrom, String::new(), name))
        }
        _ => None,
    }
}

/// Walk up from a node inside an import_statement to find the `source` field
/// (the module path string).
fn extract_module_from_ancestor(node: tree_sitter::Node, source: &str) -> String {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "import_statement" {
            if let Some(source_child) = parent.child_by_field_name("source") {
                if let Some(module_path) = node_text(source_child, source) {
                    return module_path
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string();
                }
            }
            break;
        }
        current = parent;
    }
    String::new()
}

/// Walk up from a node inside an export_statement to find the `source` field.
fn extract_export_module_from_ancestor(node: tree_sitter::Node, source: &str) -> String {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "export_statement" {
            if let Some(source_child) = parent.child_by_field_name("source") {
                if let Some(module_path) = node_text(source_child, source) {
                    return module_path
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string();
                }
            }
            break;
        }
        current = parent;
    }
    String::new()
}

/// Walk up from a node inside a `variable_declarator` whose value is a
/// `require()` call to find the module path (the first string argument).
fn extract_require_module_from_variable_declarator(
    node: tree_sitter::Node,
    source: &str,
) -> String {
    // The node is an `identifier` (the variable name). Walk up to the
    // enclosing `variable_declarator`, then descend into value → call_expression
    // → arguments → string.
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "variable_declarator" {
            if let Some(value) = parent.child_by_field_name("value") {
                if let Some(args) = value.child_by_field_name("arguments") {
                    // Find the first string child of the arguments node
                    let count = args.child_count();
                    for i in 0..count {
                        if let Some(child) = args.child(i as u32) {
                            if child.kind() == "string" {
                                if let Some(text) = node_text(child, source) {
                                    return text
                                        .trim_matches(|c| c == '"' || c == '\'')
                                        .to_string();
                                }
                            }
                        }
                    }
                }
            }
            break;
        }
        current = parent.parent();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frontend_metadata() {
        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        assert!(!spec.reference_query().is_empty());
        assert!(!spec.import_query().is_empty());
        assert!(!spec.scope_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = TypeScriptFrontendSpec;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(query.is_ok(), "definition query must compile");
    }

    #[test]
    fn declarations_capture_abstract_classes_and_members_without_duplicates() {
        let source = r#"export abstract class Base<T> {
  abstract run(value: T): void;
}
interface Result<T> { value: T; }
enum State { Ready = 'ready', Done }
export async function load(): Promise<void> {}
"#;
        let frontend = typescript_frontend();
        let facts = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("declarations.ts"),
            std::path::Path::new("declarations.ts"),
            source,
            "declarations",
            crate::ExtractionMode::Structural,
            &(),
        )
        .unwrap();

        for (qualified_name, kind) in [
            ("Base", SymbolKind::Class),
            ("Base.run", SymbolKind::Method),
            ("Result.value", SymbolKind::Property),
            ("State.Ready", SymbolKind::EnumMember),
            ("State.Done", SymbolKind::EnumMember),
        ] {
            assert_eq!(
                facts
                    .symbols
                    .iter()
                    .filter(|symbol| {
                        symbol.qualified_name == qualified_name && symbol.kind == kind
                    })
                    .count(),
                1,
                "unexpected count for {kind:?} {qualified_name}"
            );
        }
        let load = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "load")
            .expect("async load function");
        assert!(load.async_);
        assert_eq!(load.signature.as_deref(), Some("async (): Promise<void>"));
    }

    #[test]
    fn test_ref_query_parses() {
        let spec = TypeScriptFrontendSpec;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(query.is_ok(), "reference query must compile");
    }

    #[test]
    fn test_import_query_parses() {
        let spec = TypeScriptFrontendSpec;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.import_query());
        assert!(
            query.is_ok(),
            "import query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn imports_emit_one_fact_per_binding_or_side_effect() {
        let source = r#"import Client from './client';
import { foo, bar as baz } from './named';
import './side-effect';
import * as ns from './namespace';
export { greet, add as plus } from './exports';
export * from './wildcard';
const helper = require('./helper');
require('./require-effect');
class LocalService {}
export default new LocalService();"#;
        let frontend = typescript_frontend();
        let facts = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("imports.ts"),
            std::path::Path::new("imports.ts"),
            source,
            "imports",
            crate::ExtractionMode::Structural,
            &(),
        )
        .unwrap();

        assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
        assert_eq!(facts.imports.len(), 11, "{:?}", facts.imports);

        for (name, module, local_name, wildcard) in [
            ("default", "./client", Some("Client"), false),
            ("foo", "./named", None, false),
            ("bar", "./named", Some("baz"), false),
            ("", "./side-effect", None, false),
            ("ns", "./namespace", None, true),
            ("greet", "./exports", None, false),
            ("add", "./exports", Some("plus"), false),
            ("", "./wildcard", None, true),
            ("helper", "./helper", None, false),
            ("", "./require-effect", None, false),
            ("LocalService", "", Some("default"), false),
        ] {
            let import = facts
                .imports
                .iter()
                .find(|import| {
                    import.imported_name == name
                        && import.module == module
                        && import.local_name.as_deref() == local_name
                })
                .unwrap_or_else(|| panic!("missing import {name:?} from {module:?}"));
            assert_eq!(import.is_wildcard, wildcard);
            let statement =
                &source[import.range.start_byte as usize..import.range.end_byte as usize];
            assert!(statement.contains(module));
            assert!(
                statement.ends_with(';'),
                "range was not the full statement: {statement:?}"
            );
        }
    }

    #[test]
    fn test_scope_query_parses() {
        let spec = TypeScriptFrontendSpec;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_ts_definition_kind_mapping() {
        assert_eq!(
            ts_definition_kind("definition.function"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            ts_definition_kind("definition.class"),
            Some(SymbolKind::Class)
        );
        assert_eq!(ts_definition_kind("unknown.capture"), None);
    }

    #[test]
    fn test_ts_reference_kind_mapping() {
        assert_eq!(
            ts_reference_kind("reference.call"),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            ts_reference_kind("reference.field"),
            Some(ReferenceKind::FieldAccess)
        );
        assert_eq!(ts_reference_kind("unknown.capture"), None);
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = TypeScriptFrontendSpec;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_destructuring() {
        let frontend = typescript_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "function f() { const { name, value: val } = obj; }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.ts");
        let ctx = NormalizeCtx {
            language: Language::TypeScript,
            file_id,
            file_path: std::path::Path::new("test.ts"),
            source,
        };

        let mut has_name_target = false;
        let mut has_val_target = false;
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let capture_name = query.capture_names()[cap.index as usize].to_string();
            let (dn, _de) = frontend.dataflow.normalize(
                ctx,
                Capture {
                    name: capture_name,
                    node: cap.node,
                },
            );
            if let Some(dn) = dn {
                match dn.name.as_deref() {
                    Some("name") if dn.kind == DataNodeKind::Local => has_name_target = true,
                    Some("val") if dn.kind == DataNodeKind::Local => has_val_target = true,
                    _ => {}
                }
            }
        }
        assert!(
            has_name_target,
            "should create Local DataNode for shorthand destructured 'name'"
        );
        assert!(
            has_val_target,
            "should create Local DataNode for pair-pattern destructured 'val'"
        );
    }

    /// Verify that barrel re-export statements (`export * from './bar'`,
    /// `export { foo } from './bar'`) are captured as ImportDef with
    /// ImportKind::ExportFrom.
    #[test]
    fn test_export_extraction() {
        use tree_sitter::StreamingIterator;

        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();

        let source = r#"
export * from './helpers';
export { greet } from './utils';
export { add as plus } from './math';
"#;
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let query = tree_sitter::Query::new(&ts_lang, spec.import_query())
            .expect("import query (with export patterns) must compile");
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = cursor.captures(&query, root, bytes);
        let mut found_export_module = false;
        let mut found_export_name = false;

        while let Some((m, capture_index)) = captures.next() {
            if let Some(cap) = m.captures.get(*capture_index) {
                let name = &capture_names[cap.index as usize];
                match name.as_str() {
                    "export.module" => {
                        let text = cap.node.utf8_text(bytes).unwrap();
                        assert!(
                            text.contains("helpers")
                                || text.contains("utils")
                                || text.contains("math")
                        );
                        found_export_module = true;
                    }
                    "export.name" => {
                        let text = cap.node.utf8_text(bytes).unwrap();
                        assert!(!text.is_empty());
                        found_export_name = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(found_export_module, "should capture `export * from` module");
        assert!(found_export_name, "should capture named re-export");
    }

    /// Verify that CommonJS `require()` patterns are captured as imports.
    #[test]
    fn test_cjs_require_extraction() {
        use tree_sitter::StreamingIterator;

        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();

        let source = r#"
const fs = require('fs');
let path = require("path");
require('./side-effect');
"#;
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let query = tree_sitter::Query::new(&ts_lang, spec.import_query())
            .expect("import query (with CJS patterns) must compile");
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = cursor.captures(&query, root, bytes);
        let mut require_modules = Vec::new();
        let mut require_names = Vec::new();

        while let Some((m, capture_index)) = captures.next() {
            if let Some(cap) = m.captures.get(*capture_index) {
                let name = &capture_names[cap.index as usize];
                match name.as_str() {
                    "import.require_module" => {
                        let text = cap.node.utf8_text(bytes).unwrap();
                        require_modules.push(text.to_string());
                    }
                    "import.require_name" => {
                        let text = cap.node.utf8_text(bytes).unwrap();
                        require_names.push(text.to_string());
                    }
                    _ => {}
                }
            }
        }
        // Assigned require() calls emit their binding fact; only the bare
        // side-effect require emits a module-only fact.
        assert_eq!(require_modules.len(), 1);
        assert!(
            require_modules.iter().any(|m| m.contains("side-effect")),
            "should capture bare require('./side-effect') module path"
        );
        // Variable names from assigned requires: 'fs', 'path'
        assert!(
            require_names.contains(&"fs".to_string()),
            "should capture 'fs' variable name"
        );
        assert!(
            require_names.contains(&"path".to_string()),
            "should capture 'path' variable name"
        );
    }

    /// Verify that CommonJS `require()` captures normalize into correct
    /// ImportDef entries.
    #[test]
    fn test_cjs_require_normalize() {
        use tree_sitter::StreamingIterator;

        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();

        let source = "const helper = require('./helper');";
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let query = tree_sitter::Query::new(&ts_lang, spec.import_query())
            .expect("import query must compile");
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let file_id = FileId::generate("test.js");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = cursor.captures(&query, root, bytes);
        let mut imports = Vec::new();

        while let Some((m, capture_index)) = captures.next() {
            if let Some(cap) = m.captures.get(*capture_index) {
                let capture_name = &capture_names[cap.index as usize];
                if let Some(import_def) =
                    normalize_ts_import(capture_name, cap.node, source, file_id)
                {
                    imports.push(import_def);
                }
            }
        }
        assert_eq!(imports.len(), 1, "assigned require must emit one fact");
        let name_import = &imports[0];
        assert_eq!(name_import.kind, ImportKind::Import);
        assert_eq!(name_import.module, "./helper");
        assert_eq!(name_import.imported_name, "helper");
        assert!(name_import.is_relative);
        assert_eq!(name_import.range, node_range(root.named_child(0).unwrap()));
    }

    /// Verify that CommonJS `module.exports` and `exports.foo` patterns are
    /// captured as export-related imports.
    #[test]
    fn test_cjs_exports_extraction() {
        use tree_sitter::StreamingIterator;

        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();

        let source = r#"
module.exports = main;
exports.helper = helperFn;
"#;
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let query = tree_sitter::Query::new(&ts_lang, spec.import_query())
            .expect("import query (with CJS export patterns) must compile");
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = cursor.captures(&query, root, bytes);
        let mut cjs_defaults = Vec::new();
        let mut cjs_names = Vec::new();

        while let Some((m, capture_index)) = captures.next() {
            if let Some(cap) = m.captures.get(*capture_index) {
                let name = &capture_names[cap.index as usize];
                match name.as_str() {
                    "export.cjs_default" => {
                        let text = cap.node.utf8_text(bytes).unwrap();
                        cjs_defaults.push(text.to_string());
                    }
                    "export.cjs_name" => {
                        let text = cap.node.utf8_text(bytes).unwrap();
                        cjs_names.push(text.to_string());
                    }
                    _ => {}
                }
            }
        }
        assert!(
            cjs_defaults.contains(&"main".to_string()),
            "should capture module.exports = main"
        );
        assert!(
            cjs_names.contains(&"helper".to_string()),
            "should capture exports.helper = helperFn"
        );
    }

    /// Verify that CJS export captures normalize into correct ImportDef entries.
    #[test]
    fn test_cjs_exports_normalize() {
        use tree_sitter::StreamingIterator;

        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();

        let source = r#"
module.exports = handler;
exports.util = doUtil;
"#;
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let query = tree_sitter::Query::new(&ts_lang, spec.import_query())
            .expect("import query must compile");
        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let file_id = FileId::generate("test.cjs");
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = cursor.captures(&query, root, bytes);
        let mut imports = Vec::new();

        while let Some((m, capture_index)) = captures.next() {
            if let Some(cap) = m.captures.get(*capture_index) {
                let capture_name = &capture_names[cap.index as usize];
                if let Some(import_def) =
                    normalize_ts_import(capture_name, cap.node, source, file_id)
                {
                    imports.push(import_def);
                }
            }
        }
        assert!(
            !imports.is_empty(),
            "should produce ImportDef for CJS exports"
        );

        // module.exports → export.cjs_default with imported_name = "handler"
        let default_export = imports
            .iter()
            .find(|i| i.imported_name == "handler" && i.kind == ImportKind::ExportFrom)
            .expect("should have ExportFrom for module.exports = handler");
        assert!(
            default_export.module.is_empty(),
            "cjs default export has no source module"
        );

        // exports.util → export.cjs_name with imported_name = "util"
        let named_export = imports
            .iter()
            .find(|i| i.imported_name == "util" && i.kind == ImportKind::ExportFrom)
            .expect("should have ExportFrom for exports.util = doUtil");
        assert!(
            named_export.module.is_empty(),
            "cjs named export has no source module"
        );
    }
}
