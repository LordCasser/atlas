//! PHP frontend spec (slot-based).
//!
//! Provides query-driven extraction for PHP source files.
//! Supports: class, interface, trait, enum, function, method, property, constant,
//! namespace definitions; function calls, method calls, static calls, object
//! creation, field access, type references; use/require/include imports; scopes.
//!
//! Special handling: `$` prefix stripped from variable/property names;
//! namespace separator is `\`.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NoOpRecovery, NormalizeCtx, ParserSpec, ReferenceExtractorSpec,
    ScopeExtractorSpec, SymbolExtractorSpec,
};
use crate::languages::shared::{make_binding_def, make_reference_use, make_scope_def_auto_name, SymbolDefBuilder};
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// PHP frontend spec.
pub(crate) struct PhpAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_php_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = php_definition_kind(capture_name)?;
    let raw_name = node_text(node, source)?;
    // Strip `$` prefix from variable/property names
    let name = raw_name.trim_start_matches('$').to_string();
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_php("", &name, node, source);
    let signature = php_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Php, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_php_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = php_reference_kind(capture_name)?;
    let raw_text = node_text(node, source)?;
    // Strip `$` prefix for variable-like references
    let text = raw_text.trim_start_matches('$').to_string();
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_php_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = php_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let is_relative = module.starts_with('.');

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

fn normalize_php_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.namespace" => ScopeKind::Namespace,
        "scope.class" => ScopeKind::Class,
        "scope.interface" => ScopeKind::Interface,
        "scope.function" => ScopeKind::Function,
        "scope.method" => ScopeKind::Method,
        "scope.block" => ScopeKind::Block,
        "scope.conditional" => ScopeKind::Conditional,
        "scope.loop" => ScopeKind::Loop,
        _ => return None,
    };
    let range = node_range(node);

    Some(make_scope_def_auto_name(file_id, kind, range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for PhpAdapter {
    fn language(&self) -> Language {
        Language::Php
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_php::LANGUAGE_PHP.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for PhpAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/php/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/php/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_php_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for PhpAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/php/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_php_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for PhpAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/php/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_php_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for PhpAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/php/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_php_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for PhpAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/php/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.50,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_php_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for PhpAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/php/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.6,
            vec![
                "AST-driven local dataflow with language-specific gaps",
                "dynamic calls / variable-variables not resolved",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_php_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn php_frontend() -> LanguageFrontend {
    let lang = Language::Php;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(PhpAdapter),
        symbols: Box::new(PhpAdapter),
        references: Box::new(PhpAdapter),
        imports: Box::new(PhpAdapter),
        scopes: Box::new(PhpAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(PhpAdapter),
        dataflow: Box::new(PhpAdapter),
        capability: cap,
        recovery: Box::new(NoOpRecovery),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name using `\` as separator.
fn qualified_name_from_node_php(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "interface_declaration" | "trait_declaration" => {
                if let Some(type_name) = parent.child_by_field_name("name") {
                    if let Ok(type_str) = type_name.utf8_text(source.as_bytes()) {
                        parts.push(type_str.to_string());
                    }
                }
            }
            "namespace_definition" => {
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
        parts.join("\\")
    } else {
        format!("{}\\{}", prefix, parts.join("\\"))
    }
}

/// Map capture name to SymbolKind.
fn php_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.trait" => Some(SymbolKind::Trait),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.function" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.property" => Some(SymbolKind::Property),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.namespace" => Some(SymbolKind::Namespace),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn php_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn php_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let parent = node.parent()?;
            let kind = match parent.kind() {
                "require_expression" | "include_expression" => ImportKind::Include,
                _ => ImportKind::Use,
            };
            // For require/include: strip quotes
            let module = text.trim_matches(|c| c == '\'' || c == '"').to_string();
            let name = if kind == ImportKind::Use {
                module.rsplit('\\').next().unwrap_or(&module).to_string()
            } else {
                module.clone()
            };
            Some((kind, module, name))
        }
        _ => None,
    }
}

/// Extract function/method signature from the AST.
fn php_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.function" | "definition.method" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn php_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn normalize_php_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = php_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

/// Strip the `$` sigil from a PHP variable name so DataNode names are
/// consistent with definition/reference names (which already strip `$`).
fn strip_php_sigil(raw: &str) -> &str {
    raw.trim_start_matches('$')
}

fn normalize_php_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => node_text(node, source)
            .map(|raw| {
                let name = strip_php_sigil(&raw).to_string();
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "parameter",
                    Some(&name),
                    Some(&name),
                    range.start_byte,
                );
                (
                    Some(DataNode::parameter(
                        node_id, file_id, None, None, &name, range,
                    )),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.assign_target" => node_text(node, source)
            .map(|raw| {
                let name = strip_php_sigil(&raw).to_string();
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "local",
                    Some(&name),
                    Some(&name),
                    range.start_byte,
                );
                (
                    Some(DataNode::local(node_id, file_id, None, None, &name, range)),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.assign_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = crate::languages::shared::find_call_expression(
                node,
                &[
                    "function_call_expression",
                    "member_call_expression",
                    "object_creation_expression",
                ],
            )
            .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
        "df.return_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "return",
                Some(&text),
                None,
                range.start_byte,
            );
            (
                Some(DataNode {
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
                }),
                None,
            )
        }
        "df.call_target" => node_text(node, source)
            .map(|raw_name| {
                // Strip $ sigil from variable_name nodes (dynamic method calls)
                // and name nodes alike for consistent naming.
                let name = strip_php_sigil(&raw_name).to_string();
                let access_path = name.clone();
                let callsite_id = crate::languages::shared::find_call_expression(
                    node,
                    &[
                        "function_call_expression",
                        "member_call_expression",
                        "object_creation_expression",
                    ],
                )
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "call_target",
                    Some(&name),
                    Some(&access_path),
                    range.start_byte,
                );
                (
                    Some(DataNode::call_target(
                        node_id,
                        file_id,
                        None,
                        callsite_id,
                        &name,
                        &access_path,
                        range,
                    )),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let name = strip_php_sigil(&text).to_string();
            let callsite_id = crate::languages::shared::find_call_expression(
                node,
                &[
                    "function_call_expression",
                    "member_call_expression",
                    "object_creation_expression",
                ],
            )
            .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "call_arg",
                Some(&name),
                None,
                range.start_byte,
            );
            (
                Some(DataNode::call_arg(
                    node_id,
                    file_id,
                    None,
                    callsite_id,
                    Some(&name),
                    range,
                )),
                None,
            )
        }
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| {
                        p.kind() == "member_access_expression" || p.kind() == "subscript_expression"
                    })
                    .and_then(|p| node_text(p, source))
                    .unwrap_or_else(|| name.clone());
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
                    "field",
                    Some(&name),
                    Some(&access_path),
                    range.start_byte,
                );
                (
                    Some(DataNode::field(
                        node_id,
                        file_id,
                        None,
                        &name,
                        &access_path,
                        range,
                    )),
                    None,
                )
            })
            .unwrap_or((None, None)),
        "df.receiver" | "df.literal" => {
            let text = node_text(node, source).unwrap_or_default();
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
            (
                Some(DataNode {
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
                }),
                None,
            )
        }
        // ── PHP dataflow additions (§2.11) ────────────────────────
        "df.index" => {
            // Index expression in $arr[$key] → Expr DataNode
            let text = node_text(node, source).unwrap_or_default();
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
                    callsite_id: None,
                    name: Some(text),
                    access_path: None,
                    arg_index: None,
                    range,
                }),
                None,
            )
        }
        "df.assign_field_target" => {
            // Array assignment LHS: $arr[$key] = value → Field DataNode
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "field",
                Some(&text),
                Some(&text),
                range.start_byte,
            );
            (
                Some(DataNode::field(node_id, file_id, None, &text, &text, range)),
                None,
            )
        }
        "df.superglobal" => {
            // $_GET, $_POST, etc. → Global DataNode
            node_text(node, source)
                .map(|name| {
                    if name.starts_with("$_") {
                        let node_id = DataNodeId::generate(
                            &file_id,
                            None::<&SymbolId>,
                            "global",
                            Some(&name),
                            Some(&name),
                            range.start_byte,
                        );
                        (
                            Some(DataNode {
                                id: node_id,
                                file_id,
                                function_id: None,
                                kind: DataNodeKind::Global,
                                binding_id: None,
                                callsite_id: None,
                                name: Some(name),
                                access_path: None,
                                arg_index: None,
                                range,
                            }),
                            None,
                        )
                    } else {
                        (None, None)
                    }
                })
                .unwrap_or((None, None))
        }
        "df.identifier_use" => {
            // Filter out declaration contexts and superglobals
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &["namespace_use_clause", "use_declaration"],
            ) {
                return (None, None);
            }
            // Skip left-hand side of assignment (already captured as df.assign_target)
            if let Some(parent) = node.parent() {
                if parent.kind() == "assignment_expression"
                    && parent
                        .child_by_field_name("left")
                        .is_some_and(|n| n.id() == node.id())
                {
                    return (None, None);
                }
            }
            // Skip superglobals (already captured as df.superglobal)
            let text = node_text(node, source).unwrap_or_default();
            if text.is_empty() || text.starts_with("$_") {
                return (None, None);
            }
            let name = strip_php_sigil(&text).to_string();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "identifier_use",
                Some(&name),
                Some(&name),
                range.start_byte,
            );
            let dn = DataNode {
                id: node_id,
                file_id,
                function_id: None,
                kind: DataNodeKind::VariableUse,
                binding_id: None,
                callsite_id: None,
                name: Some(name.clone()),
                access_path: Some(name),
                arg_index: None,
                range,
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
        let spec = PhpAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = PhpAdapter;
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
        let spec = PhpAdapter;
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
        let spec = PhpAdapter;
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
        let spec = PhpAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = PhpAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_php() {
        let frontend = php_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = r#"<?php
function f($req) {
    $name = $_GET["name"];
    $clean = sanitize($name);
    return $clean;
}
"#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.php");
        let ctx = NormalizeCtx {
            language: Language::Php,
            file_id,
            file_path: std::path::Path::new("test.php"),
            source,
        };

        let mut has_global = false;
        let mut has_receiver = false;
        let mut has_expr = false;
        let mut has_parameter = false;
        let mut has_return = false;
        let mut has_local = false;
        let mut has_call_target = false;
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
                match dn.kind {
                    DataNodeKind::Global => has_global = true,
                    DataNodeKind::Receiver => has_receiver = true,
                    DataNodeKind::Expr => has_expr = true,
                    DataNodeKind::Parameter => has_parameter = true,
                    DataNodeKind::Return => has_return = true,
                    DataNodeKind::Local => has_local = true,
                    DataNodeKind::CallTarget => has_call_target = true,
                    _ => {}
                }
            }
        }
        assert!(has_global, "should have Global DataNode for $_GET");
        assert!(
            has_receiver,
            "should have Receiver DataNode for $_GET in $_GET['name']"
        );
        assert!(has_expr, "should have Expr DataNode for array index");
        assert!(has_parameter, "should have Parameter DataNode for $req");
        assert!(has_return, "should have Return DataNode");
        assert!(has_local, "should have Local DataNode for $name/$clean");
        assert!(
            has_call_target,
            "should have CallTarget DataNode for sanitize"
        );
    }
}
