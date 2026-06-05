//! Java frontend spec (slot-based).
//!
//! Provides query-driven extraction for Java source files.
//! Supports: class, interface, enum, method, field, constant, variable definitions;
//! method calls, field access, type references; import/include; scopes.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NoOpRecovery, NormalizeCtx, ParserSpec, ReferenceExtractorSpec,
    ScopeExtractorSpec, SymbolExtractorSpec,
};
use crate::languages::shared::SymbolDefBuilder;
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Java frontend spec.
pub(crate) struct JavaAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_java_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = java_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_java("", &name, node, source);
    let signature = java_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Java, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_java_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = java_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    // source_symbol is resolved by SemanticBinder after extraction.
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

fn normalize_java_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = java_import_info(capture_name, node, source)?;
    let range = node_range(node);
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
        imported_name: imported_name.clone(),
        local_name: Some(imported_name),
        is_wildcard,
        is_relative: false, // Java imports are always absolute
        range,
        alias: None,
    })
}

fn normalize_java_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.class" => ScopeKind::Class,
        "scope.interface" => ScopeKind::Interface,
        "scope.enum" => ScopeKind::Enum,
        "scope.method" => ScopeKind::Method,
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

impl ParserSpec for JavaAdapter {
    fn language(&self) -> Language {
        Language::Java
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for JavaAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/java/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/java/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_java_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for JavaAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/java/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_java_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for JavaAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/java/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_java_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for JavaAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/java/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_java_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for JavaAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/java/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.65,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_java_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for JavaAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/java/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.65,
            vec!["AST-driven local dataflow with language-specific gaps"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_java_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn java_frontend() -> LanguageFrontend {
    let lang = Language::Java;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(JavaAdapter),
        symbols: Box::new(JavaAdapter),
        references: Box::new(JavaAdapter),
        imports: Box::new(JavaAdapter),
        scopes: Box::new(JavaAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(JavaAdapter),
        dataflow: Box::new(JavaAdapter),
        capability: cap,
        recovery: Box::new(NoOpRecovery),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name for a Java symbol from its parent hierarchy.
fn qualified_name_from_node_java(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                if let Some(child) = parent.child_by_field_name("name") {
                    if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                        parts.push(class_name.to_string());
                    }
                }
            }
            _ => {}
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

/// Map capture name to SymbolKind.
fn java_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.method" => Some(SymbolKind::Method),
        "definition.field" => Some(SymbolKind::Field),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn java_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.instantiation" => Some(ReferenceKind::Instantiation),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

/// Extract import info from capture.
fn java_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let path = node_text(node, source)?;
            // Last segment is the imported name
            let name = path.rsplit('.').next().unwrap_or(&path).to_string();
            Some((ImportKind::Import, path, name))
        }
        "import.wildcard" => {
            let path = node_text(node, source)?;
            let module = path.trim_end_matches(".*").to_string();
            Some((ImportKind::FromImport, module, "*".to_string()))
        }
        _ => None,
    }
}

/// Extract method/constructor signature (formal parameters) from the AST.
///
/// The `node` is the identifier captured by `@definition.method` or
/// `@definition.constructor`. Its parent is `method_declaration` or
/// `constructor_declaration`, which has a `formal_parameters` child.
fn java_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.method" | "definition.constructor" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

/// Map capture name to BindingKind.
fn java_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

/// Normalize a lexical capture into a BindingDef.
fn normalize_java_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = java_binding_kind(capture_name)?;
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

// ── Dataflow normalize ─────────────────────────────────────────────────

/// Find a call expression ancestor for a dataflow node.
/// Normalize a dataflow capture into (DataNode, DataFlowEdge).
fn normalize_java_dataflow_builder(
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
                    None::<&SymbolId>,
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
                    None::<&SymbolId>,
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
            let callsite_id = crate::languages::shared::find_call_expression(node, &["method_invocation", "object_creation_expression"])
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "expr",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
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
            };
            (Some(dn), None)
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
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = name.clone();
                let callsite_id = crate::languages::shared::find_call_expression(node, &["method_invocation", "object_creation_expression"]).map(|ce| {
                    types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32)
                });
                let node_id = DataNodeId::generate(
                    &file_id,
                    None::<&SymbolId>,
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
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = crate::languages::shared::find_call_expression(node, &["method_invocation", "object_creation_expression"])
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "call_arg",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode::call_arg(node_id, file_id, None, callsite_id, Some(&text), range);
            (Some(dn), None)
        }
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "field_access")
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
                let dn = DataNode::field(node_id, file_id, None, &name, &access_path, range);
                (Some(dn), None)
            })
            .unwrap_or((None, None)),
        "df.assign_field_target" => {
            // Node is a field_access (e.g. "obj.field" or "this.field").
            // Create a Field DataNode with the full text as name and access_path.
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
        "df.index" => {
            // Node is the index expression of an array access (e.g. arr[i]),
            // could be an identifier, literal, or complex expression.
            let text = node_text(node, source).unwrap_or_default();
            let node_id = DataNodeId::generate(
                &file_id,
                None::<&SymbolId>,
                "expr",
                Some(&text),
                None,
                range.start_byte,
            );
            let dn = DataNode {
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
            };
            (Some(dn), None)
        }
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
        "df.identifier_use" => {
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &[
                    "object_creation_expression",
                    "type_identifier",
                    "method_invocation",
                ],
            ) {
                return (None, None);
            }
            let text = node_text(node, source).unwrap_or_default();
            if text.is_empty() {
                return (None, None);
            }
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
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let spec = JavaAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = JavaAdapter;
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
        let spec = JavaAdapter;
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
        let spec = JavaAdapter;
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
        let spec = JavaAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_lexical_query_parses() {
        let spec = JavaAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.lexical_query());
        assert!(
            query.is_ok(),
            "lexical query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = JavaAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_lexical_normalize_parameter() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void bar(int x) {} }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query = tree_sitter::Query::new(&ts_lang, frontend.lexical.lexical_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut hits = 0;
        let mut captures = cursor.captures(&query, root, source.as_bytes());
        use tree_sitter::StreamingIterator;
        while let Some((m, idx)) = captures.next() {
            let cap = m.captures[*idx];
            let name = query.capture_names()[cap.index as usize].to_string();
            if frontend
                .lexical
                .normalize(
                    ctx,
                    Capture {
                        name,
                        node: cap.node,
                    },
                )
                .is_some()
            {
                hits += 1;
            }
        }
        assert!(
            hits > 0,
            "lexical query should produce at least one BindingDef for int x parameter"
        );
    }

    #[test]
    fn test_dataflow_normalize_assignment() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { int bar() { int x = 1; x = 2; return x; } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut node_hits = 0;
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
            if dn.is_some() {
                node_hits += 1;
            }
        }
        assert!(
            node_hits > 0,
            "dataflow query should produce at least one DataNode for assignment + return"
        );
    }

    #[test]
    fn test_dataflow_call_arg_normalize() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void bar() { helper(x, 42); } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut has_call_arg = false;
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
                    DataNodeKind::CallArg => has_call_arg = true,
                    DataNodeKind::CallTarget => has_call_target = true,
                    _ => {}
                }
            }
        }
        assert!(has_call_target, "should have a call target (helper)");
        assert!(has_call_arg, "should have call arguments (x, 42)");
    }

    #[test]
    fn test_dataflow_field_assignment_and_object_creation() {
        let frontend = java_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "class Foo { void bar() { obj.field = 42; Foo f = new Foo(x, y); } }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.java");
        let ctx = NormalizeCtx {
            language: Language::Java,
            file_id,
            file_path: std::path::Path::new("Test.java"),
            source,
        };

        let mut field_nodes: Vec<DataNode> = Vec::new();
        let mut call_target_nodes: Vec<DataNode> = Vec::new();
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
                    DataNodeKind::Field => field_nodes.push(dn),
                    DataNodeKind::CallTarget => call_target_nodes.push(dn),
                    _ => {}
                }
            }
        }

        // Should have at least one Field node from "obj.field" assignment
        assert!(
            !field_nodes.is_empty(),
            "should have at least one Field DataNode from obj.field assignment"
        );
        let field_texts: Vec<&str> = field_nodes
            .iter()
            .filter_map(|n| n.name.as_deref())
            .collect();
        assert!(
            field_texts.iter().any(|t| t.contains("field")),
            "should capture field assignment, got: {field_texts:?}"
        );

        // Should have a CallTarget DataNode from "new Foo()"
        let call_names: Vec<&str> = call_target_nodes
            .iter()
            .filter_map(|n| n.name.as_deref())
            .collect();
        assert!(
            call_names.iter().any(|t| t.contains("Foo")),
            "should capture new Foo(x, y) call target, got: {call_names:?}"
        );
    }
}
