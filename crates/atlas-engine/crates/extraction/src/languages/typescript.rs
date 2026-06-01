//! TypeScript / JavaScript frontend spec (slot-based).
//!
//! Uses tree-sitter-typescript grammar and embedded query files.
//! JavaScript is treated as a subset of TypeScript for extraction purposes.
//!
//! `TypeScriptFrontendSpec` implements every slot trait (ParserSpec through
//! DataflowSpec) via shared private normalize helpers.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, ImportExtractorSpec, LanguageFrontend, LexicalBindingSpec, NoOpRecovery,
    NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec, SymbolExtractorSpec,
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
    let range = node_range(node);

    let qualified_name = qualified_name_from_node("", &name, node, source);
    let exported = is_exported_in_tree(node);

    Some(
        SymbolDefBuilder::new(file_id, language, kind, name, qualified_name, range)
            .exported(exported)
            .build(),
    )
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

pub(crate) fn normalize_ts_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = ts_import_info(capture_name, node, source)?;
    let range = node_range(node);

    // For aliased imports/exports, the captured node is the alias (e.g. `bar` in
    // `import { foo as bar }`), but imported_name should hold the original exported
    // name. Walk up to the parent specifier's `name` field to get it.
    let (imported_name, local_name) =
        if capture_name == "import.alias" || capture_name == "export.alias" {
            // parent is import_specifier or export_specifier — both have a `name` field
            let original = node
                .parent()
                .and_then(|p| p.child_by_field_name("name"))
                .and_then(|n| node_text(n, source))
                .unwrap_or_else(|| imported_name.clone());
            let alias = imported_name.clone(); // this is the alias text from ts_import_info
            (original, alias)
        } else {
            let local = imported_name.clone();
            (imported_name, local)
        };
    let is_relative = module.starts_with('.');
    let is_wildcard = capture_name.contains("wildcard")
        // `export.module` without an accompanying `export.name` = wildcard re-export
        || (capture_name == "export.module");

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
        local_name: Some(local_name),
        is_wildcard,
        is_relative,
        range,
        alias: None,
    })
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

pub(crate) fn normalize_ts_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = ts_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    let scope_id = types::ids::ScopeId::generate(
        &file_id,
        None::<&types::ids::ScopeId>,
        kind.as_str(),
        range.start_byte,
    );
    let id = types::ids::BindingId::generate(
        &file_id,
        &scope_id,
        kind.as_str(),
        &name,
        range.start_byte,
    );
    Some(BindingDef {
        id,
        file_id,
        function_id: None,
        scope_id,
        kind,
        name,
        symbol_id: None,
        range,
    })
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
        | "method_definition"
        | "interface_declaration"
        | "enum_declaration"
        | "type_alias_declaration"
        | "module"
        | "import_specifier"
        | "import_clause"
        | "namespace_import"
        | "catch_clause"
        | "public_field_definition"
        | "required_parameter"
        | "optional_parameter" => {
            // Check if this node is the "name" field of the parent
            parent
                .child_by_field_name("name")
                .map_or(false, |n| n.id() == node.id())
        }
        // Property names in member expressions (obj.property)
        "member_expression" => parent
            .child_by_field_name("property")
            .map_or(false, |n| n.id() == node.id()),
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
        "df.parameter" => node_text(node, source)
            .map(|name| {
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&types::ids::SymbolId>,
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
                    None::<&types::ids::SymbolId>,
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
            let callsite_id = find_call_expression(node)
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "expr",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::Expr,
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
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "return",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: types::enums::DataNodeKind::Return,
                binding_id: None,
                callsite_id: None,
                name: Some(text),
                access_path: None,
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression(node)
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "call_arg",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode::call_arg(node_id, file_id, None, callsite_id, Some(&text), range);
            (Some(dn), None)
        }
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "member_expression")
                    .and_then(|p| node_text(p, source))
                    .unwrap_or_else(|| name.clone());
                let callsite_id = find_call_expression(node).map(|ce| {
                    types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32)
                });
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
            let callsite_id = find_call_expression(node)
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
            let name = text.clone();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "field",
                Some(&name),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode::field(node_id, file_id, None, &name, &text, range);
            (Some(dn), None)
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
        recovery: Box::new(NoOpRecovery),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk up the AST to find the enclosing `call_expression` or `new_expression`, if any.
///
/// Used to group `CallArg` and `CallTarget` nodes that belong to the same
/// call site — this enables correct ArgToParam edge creation even in the
/// presence of nested calls like `foo(bar(a), b)`.
fn find_call_expression(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    // A call expression can be captured directly (e.g., @df.assign_value
    // on a call_expression) or as a child (e.g., @df.call_arg).
    let mut current = node;
    if current.kind() == "call_expression" || current.kind() == "new_expression" {
        return Some(current);
    }
    while let Some(parent) = current.parent() {
        if parent.kind() == "call_expression" || parent.kind() == "new_expression" {
            return Some(parent);
        }
        current = parent;
    }
    None
}

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
            "class_declaration" | "class" => {
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
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
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
        "import.name" | "import.alias" => {
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
}
