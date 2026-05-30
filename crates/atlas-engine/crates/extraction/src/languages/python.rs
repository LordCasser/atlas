//! Python frontend spec (slot-based).
//!
//! Uses tree-sitter-python grammar and embedded query files.

use crate::languages::{node_range, node_text};
use types::*;

use crate::frontend::{
    Capture, DataflowSpec, ImportExtractorSpec, LanguageFrontend, LexicalBindingSpec,
    NoOpRecovery, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use types::capability::FeatureSupport;

// ---------------------------------------------------------------------------
// Frontend spec struct
// ---------------------------------------------------------------------------

/// Python frontend spec.
pub(crate) struct PythonAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_py_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    use super::shared::SymbolDefBuilder;

    let kind = py_definition_kind(capture_name, node)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_py("", &name, node, source);
    let exported = is_exported_in_tree_py(node, &name);
    let signature = py_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Python, kind, name, qualified_name, range)
            .signature(signature)
            .exported(exported)
            .build(),
    )
}

fn normalize_py_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = py_reference_kind(capture_name)?;
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

fn normalize_py_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name, is_relative) = py_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let local_name = imported_name.clone();
    let is_wildcard = capture_name.contains("wildcard");

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

fn normalize_py_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    _source: &str,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.function" => ScopeKind::Function,
        "scope.class" => ScopeKind::Class,
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

/// Check if an identifier is a declaration name or property name in Python AST.
fn is_py_identifier_declaration(node: tree_sitter::Node) -> bool {
    let parent = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    match parent.kind() {
        "function_definition"
        | "class_definition"
        | "parameters"
        | "aliased_import"
        | "import_statement"
        | "except_clause"
        | "with_item"
        | "for_statement" => {
            // Check if this node is the "name" field of the parent
            parent
                .child_by_field_name("name")
                .map_or(false, |n| n.id() == node.id())
        }
        // Attribute access: obj.attr — don't capture property name as use
        "attribute" => parent
            .child_by_field_name("attribute")
            .map_or(false, |n| n.id() == node.id()),
        _ => false,
    }
}

/// Find the enclosing `call` node in Python AST.
fn find_call_expression_python(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    // Check current node first — the captured node may itself be the call expression
    // (e.g. when `df.assign_value` captures a call node directly).
    if node.kind() == "call" {
        return Some(node);
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "call" {
            return Some(parent);
        }
        current = parent;
    }
    None
}

fn normalize_py_dataflow_builder(
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
            let callsite_id = find_call_expression_python(node)
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
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&types::ids::SymbolId>,
                "return",
                None,
                None,
                range.start_byte,
            );
            let dn = DataNode::return_(node_id, file_id, None, range);
            (Some(dn), None)
        }
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = find_call_expression_python(node)
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
                    .filter(|p| p.kind() == "attribute")
                    .and_then(|p| node_text(p, source))
                    .unwrap_or_else(|| name.clone());
                let callsite_id = find_call_expression_python(node).map(|ce| {
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
        "df.field_name" => {
            node_text(node, source)
                .map(|name| {
                    // Build full access_path from parent attribute node
                    // e.g. for "request.args" → access_path = "request.args"
                    let access_path = node
                        .parent()
                        .filter(|p| p.kind() == "attribute")
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
                .unwrap_or((None, None))
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
        "df.identifier_use" => {
            // Skip identifiers that are declaration names or property names
            if is_py_identifier_declaration(node) {
                return (None, None);
            }
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "identifier_use",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: DataNodeKind::VariableUse,
                binding_id: None,
                callsite_id: None,
                name: Some(text.clone()),
                access_path: Some(text),
                arg_index: None,
                range,
            };
            (Some(dn), None)
        }
        "df.assign_field_target" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "field",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            let dn = DataNode::field(node_id, file_id, None, &text, &text, range);
            (Some(dn), None)
        }
        _ => (None, None),
    }
}

// ---------------------------------------------------------------------------
// Slot trait implementations — each calls the private normalize_py_* helpers.
// ---------------------------------------------------------------------------

impl ParserSpec for PythonAdapter {
    fn language(&self) -> Language {
        Language::Python
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for PythonAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/python/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/python/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_py_definition(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl ReferenceExtractorSpec for PythonAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/python/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_py_reference(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl ImportExtractorSpec for PythonAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/python/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_py_import(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl ScopeExtractorSpec for PythonAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/python/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_py_scope(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl LexicalBindingSpec for PythonAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/python/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec![
                "scope-chain-aware binding with shadowing support; assignment LHS treated as definition",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_py_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for PythonAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/python/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["Dataflow extraction for Python is experimental"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_py_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

/// Construct a [`LanguageFrontend`] directly from Python-specific slot
/// implementations — no adapter wrapper needed.
/// This is the canonical Python frontend factory.
pub(crate) fn python_frontend() -> LanguageFrontend {
    use crate::callsite_spec::create_extractor;
    use crate::frontend::FrontendParts;
    use types::capability::LanguageCapabilityProfile;

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(PythonAdapter),
        symbols: Box::new(PythonAdapter),
        references: Box::new(PythonAdapter),
        imports: Box::new(PythonAdapter),
        scopes: Box::new(PythonAdapter),
        callsites: create_extractor(Language::Python),
        lexical: Box::new(PythonAdapter),
        dataflow: Box::new(PythonAdapter),
        capability: LanguageCapabilityProfile::for_language(Language::Python),
        recovery: Box::new(NoOpRecovery),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a Python symbol.
fn qualified_name_from_node_py(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node;

    while let Some(parent) = current.parent() {
        if parent.kind() == "class_definition" {
            if let Some(child) = parent.child_by_field_name("name") {
                if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                    // Skip if the class name equals the current segment's name
                    // to avoid double-counting when starting from the name child.
                    if class_name != name {
                        parts.push(class_name.to_string());
                    }
                }
            }
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

/// Extract project name from pyproject.toml (simple parser for MVP).
#[allow(dead_code)]
fn extract_toml_project_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name ") {
            if let Some(rest) = rest.strip_prefix('=') {
                let name = rest.trim().trim_matches('"').trim_matches('\'');
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Check if a Python definition is exported (module-level, no leading underscore).
fn is_exported_in_tree_py(node: tree_sitter::Node, name: &str) -> bool {
    if name.starts_with('_') {
        return false;
    }
    // Walk up to check if we're at the module (file) scope
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "module" => return true,                          // Top-level definition
            "class_definition" => return true,                // Class member (public by convention)
            "function_definition" | "lambda" => return false, // Nested in function → not exported
            _ => {}
        }
        current = parent;
    }
    false
}

/// Map capture name to SymbolKind.
fn py_definition_kind(capture: &str, node: tree_sitter::Node) -> Option<SymbolKind> {
    match capture {
        "definition.function" => {
            // A function_definition inside a class_definition is a method.
            // `node` is the identifier; walk up from its parent (function_definition).
            let mut cursor = node.parent(); // function_definition
            while let Some(p) = cursor {
                match p.kind() {
                    "class_definition" => return Some(SymbolKind::Method),
                    "module" => break,
                    _ => cursor = p.parent(),
                }
            }
            Some(SymbolKind::Function)
        }
        "definition.class" => Some(SymbolKind::Class),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn py_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.decorator" => Some(ReferenceKind::Decoration),
        "reference.usage" => Some(ReferenceKind::Usage),
        _ => None,
    }
}

/// Extract function/method signature (parameter list) from the AST.
///
/// The `node` is the identifier captured by `@definition.function` or `@definition.class`.
/// For functions/methods, we walk to the parent `function_definition` and extract its
/// `parameters` child. For classes, we look for `__init__` parameters.
fn py_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" => {
            // node is the identifier; parent is function_definition
            let func_def = node.parent()?;
            if func_def.kind() != "function_definition" {
                return None;
            }
            let params = func_def.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

/// Extract import info from capture.
fn py_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String, bool)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let is_relative = text.starts_with('.');
            Some((ImportKind::Import, text, String::new(), is_relative))
        }
        "import.name" => {
            let name = node_text(node, source)?;
            let module = extract_module_from_import_ancestor(node, source);
            Some((ImportKind::Import, module, name, false))
        }
        "import.alias" => {
            let name = node_text(node, source)?;
            let module = extract_module_from_import_ancestor(node, source);
            Some((ImportKind::Import, module, name, false))
        }
        "import.wildcard" => {
            let module = extract_module_from_import_ancestor(node, source);
            Some((ImportKind::FromImport, module, "*".into(), false))
        }
        _ => None,
    }
}

/// Walk up from a node inside an import_statement/import_from_statement
/// to find the module name (either from the `name` field or dotted_name).
fn extract_module_from_import_ancestor(node: tree_sitter::Node, source: &str) -> String {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "import_statement" => {
                // `import foo` — module is the name/dotted_name field
                if let Some(name_child) = parent.child_by_field_name("name") {
                    if let Some(m) = node_text(name_child, source) {
                        return m;
                    }
                }
                break;
            }
            "import_from_statement" => {
                // `from foo import bar` — module is the module_name field
                if let Some(module_name) = parent.child_by_field_name("module_name") {
                    if let Some(m) = node_text(module_name, source) {
                        return m;
                    }
                }
                break;
            }
            _ => {}
        }
        current = parent;
    }
    String::new()
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn py_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        "lexical.import_alias" => Some(BindingKind::ImportAlias),
        _ => None,
    }
}

/// Normalize a Python lexical capture into a [`BindingDef`].
///
/// **Known limitation**: every assignment LHS is treated as a new binding
/// definition.  For repeated assignments to the same name in one scope
/// (`x = a; x = b`), the second assignment creates a separate BindingDef
/// rather than being recognised as a rebind of the existing variable.  This
/// means scope-aware use-def may over‑approximate for dynamically‑typed
/// reassignment chains.  Fixing this requires per‑name deduplication in the
/// lexical binder.
fn normalize_py_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = py_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);
    let id = BindingId::generate(&file_id, &scope_id, kind.as_str(), &name, range.start_byte);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frontend_metadata() {
        let spec = PythonAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        assert!(!spec.reference_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = PythonAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(query.is_ok(), "Python def query must compile");
    }

    #[test]
    fn test_ref_query_parses() {
        let spec = PythonAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(query.is_ok(), "Python ref query must compile");
    }

    #[test]
    fn test_py_definition_kind_basic() {
        // We can't easily construct tree-sitter Nodes in unit tests,
        // so test the capture-name mapping only (without AST parent check).
        // The Method detection via parent walk is tested implicitly by the
        // integration test pipeline. Here we verify the fallback behavior:
        // when node has no class_definition parent, "definition.function" → Function.
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();

        // Top-level function → Function
        let tree = parser.parse("def foo(): pass", None).unwrap();
        let root = tree.root_node();
        let func_node = root.child(0).unwrap().child_by_field_name("name").unwrap();
        assert_eq!(
            py_definition_kind("definition.function", func_node),
            Some(SymbolKind::Function)
        );

        // Method (function inside class) → Method
        let tree = parser
            .parse("class Foo:\n    def bar(self): pass", None)
            .unwrap();
        let root = tree.root_node();
        // class → body → block → function_definition → name
        let class_node = root.child(0).unwrap();
        let body = class_node.child_by_field_name("body").unwrap();
        let func_def = body.child(0).unwrap();
        let method_name = func_def.child_by_field_name("name").unwrap();
        assert_eq!(
            py_definition_kind("definition.function", method_name),
            Some(SymbolKind::Method)
        );

        // Class → Class
        let class_name = class_node.child_by_field_name("name").unwrap();
        assert_eq!(
            py_definition_kind("definition.class", class_name),
            Some(SymbolKind::Class)
        );

        // Unknown → None
        assert_eq!(py_definition_kind("unknown", func_node), None);
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = super::python_frontend();
        let lang = spec.parser.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_smoke() {
        let frontend = python_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = r#"def foo(x):
    y = x.field
    result = bar(y, 42)
    return result
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.ext");
        let ctx = NormalizeCtx {
            language: frontend.parser.language(),
            file_id,
            file_path: std::path::Path::new("test.ext"),
            source,
        };

        let mut nodes: Vec<DataNode> = Vec::new();
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            let (dn, _de) = frontend.dataflow.normalize(
                ctx,
                Capture {
                    name,
                    node: cap.node,
                },
            );
            if let Some(dn) = dn {
                nodes.push(dn);
            }
        }

        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::Parameter),
            "param"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::CallTarget),
            "calltarget"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::CallArg),
            "callarg"
        );
        assert!(
            nodes.iter().any(|n| n.kind == DataNodeKind::VariableUse),
            "varuse"
        );
    }
}
