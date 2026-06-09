//! Ruby frontend spec (slot-based).
//!
//! Provides query-driven extraction for Ruby source files.
//! Supports: class, module, method, constant, variable, field (attr_*)
//! definitions; method calls, constant references; require/include/extend
//! imports; scopes.
//!
//! Note: singleton methods (`def self.method`) are captured as Method, not
//! differentiated from instance methods at the Symbolic level.
//!
//! ## Known gaps (documented, not yet implemented)
//!
//! - **Block/yield implicit calls**: `do |params| ... end` blocks passed to
//!   method calls are not modeled as virtual callsites.  `yield(args)` does
//!   not create dataflow edges to the calling context.  This means dataflow
//!   tracing stops at block boundaries and yield is treated as a sink.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NoOpRecovery, NormalizeCtx, ParserSpec, ReferenceExtractorSpec,
    ScopeExtractorSpec, SymbolExtractorSpec,
};
use crate::languages::shared::{
    SymbolDefBuilder, make_binding_def, make_df_assign_field_target, make_df_parameter,
    make_df_return_value, make_reference_use, make_scope_def_auto_name,
};
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Ruby frontend spec.
pub(crate) struct RubyAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_ruby_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = ruby_definition_kind(capture_name)?;
    let raw_name = node_text(node, source)?;
    // For symbols `:name`, strip the leading `:`
    let name = raw_name.trim_start_matches(':').to_string();
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_ruby("", &name, node, source);
    let signature = ruby_extract_signature(capture_name, node, source);

    Some(
        SymbolDefBuilder::new(file_id, Language::Ruby, kind, name, qualified_name, range)
            .signature(signature)
            .build(),
    )
}

fn normalize_ruby_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = ruby_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    Some(make_reference_use(file_id, kind, text, name, range))
}

fn normalize_ruby_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = ruby_import_info(capture_name, node, source)?;
    let range = node_range(node);

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
        is_relative: false,
        range,
        alias: None,
    })
}

fn normalize_ruby_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.module" => ScopeKind::Module,
        "scope.class" => ScopeKind::Class,
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

impl ParserSpec for RubyAdapter {
    fn language(&self) -> Language {
        Language::Ruby
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_ruby::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for RubyAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/ruby/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/ruby/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_ruby_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for RubyAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/ruby/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_ruby_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for RubyAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/ruby/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_ruby_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for RubyAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/ruby/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_ruby_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for RubyAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/ruby/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.45,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_ruby_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for RubyAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/ruby/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec![
                "implicit return is approximate (body_statement last-child heuristic)",
                "method calls and field access share `call` node; attr_reader not resolved",
                "dynamic methods / method_missing not resolved",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_ruby_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn ruby_frontend() -> LanguageFrontend {
    let lang = Language::Ruby;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(RubyAdapter),
        symbols: Box::new(RubyAdapter),
        references: Box::new(RubyAdapter),
        imports: Box::new(RubyAdapter),
        scopes: Box::new(RubyAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(RubyAdapter),
        dataflow: Box::new(RubyAdapter),
        capability: cap,
        recovery: Box::new(NoOpRecovery),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Infer a qualified name using `::` for modules/classes and `#` for methods.
fn qualified_name_from_node_ruby(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class" | "module" => {
                if let Some(type_name) = parent.child_by_field_name("name") {
                    if let Ok(type_str) = type_name.utf8_text(source.as_bytes()) {
                        parts.push(type_str.to_string());
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }

    parts.reverse();
    if prefix.is_empty() {
        parts.join("::")
    } else {
        format!("{}::{}", prefix, parts.join("::"))
    }
}

/// Map capture name to SymbolKind.
fn ruby_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.module" => Some(SymbolKind::Module),
        "definition.method" => Some(SymbolKind::Method),
        "definition.constant" => Some(SymbolKind::Constant),
        "definition.variable" => Some(SymbolKind::Variable),
        "definition.field" => Some(SymbolKind::Field), // attr_*
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn ruby_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

/// Extract import info from capture.
fn ruby_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '\'' || c == '"').to_string();
            // Determine kind from ancestor call method name
            let method_name = find_ancestor_method_name(node, source)?;
            let kind = match method_name.as_str() {
                "require" | "require_relative" => ImportKind::Import,
                "include" | "extend" | "prepend" => ImportKind::Include,
                _ => ImportKind::Import,
            };
            Some((kind, cleaned.clone(), cleaned))
        }
        _ => None,
    }
}

/// Find the method name from the ancestor `call` node.
fn find_ancestor_method_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "call" {
            if let Some(method) = parent.child_by_field_name("method") {
                return method
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(|s| s.to_string());
            }
        }
        current = parent.parent();
    }
    None
}

/// Extract method signature from the AST.
fn ruby_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    match capture_name {
        "definition.method" => {
            let parent = node.parent()?;
            let params = parent.child_by_field_name("parameters")?;
            Some(node_text(params, source)?)
        }
        _ => None,
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn ruby_binding_kind(capture_name: &str) -> Option<BindingKind> {
    match capture_name {
        "lexical.parameter" => Some(BindingKind::Parameter),
        "lexical.local" => Some(BindingKind::Local),
        "lexical.catch_variable" => Some(BindingKind::CatchVariable),
        _ => None,
    }
}

fn normalize_ruby_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    let kind = ruby_binding_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_ruby_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    let range = node_range(node);
    match capture_name {
        "df.parameter" => make_df_parameter(file_id, node, source, range),
        "df.assign_target" => {
            // Differentiate by AST node kind: identifier → Local,
            // instance_variable (@x) → Field, class_variable (@@x) → Field,
            // global_variable ($x) → Global
            let text = node_text(node, source).unwrap_or_default();
            let (kind_str, dn) = match node.kind() {
                "instance_variable" | "class_variable" => {
                    let node_id = DataNodeId::generate(
                        &file_id,
                        None::<&SymbolId>,
                        "field",
                        Some(&text),
                        Some(&text),
                        range.start_byte,
                    );
                    (
                        "field",
                        DataNode::field(node_id, file_id, None, &text, &text, range),
                    )
                }
                "global_variable" => {
                    let node_id = DataNodeId::generate(
                        &file_id,
                        None::<&SymbolId>,
                        "global",
                        Some(&text),
                        Some(&text),
                        range.start_byte,
                    );
                    (
                        "global",
                        DataNode {
                            id: node_id,
                            file_id,
                            function_id: None,
                            kind: DataNodeKind::Global,
                            binding_id: None,
                            callsite_id: None,
                            name: Some(text.clone()),
                            access_path: Some(text),
                            arg_index: None,
                            range,
                        },
                    )
                }
                _ => {
                    let node_id = DataNodeId::generate(
                        &file_id,
                        None::<&SymbolId>,
                        "local",
                        Some(&text),
                        Some(&text),
                        range.start_byte,
                    );
                    (
                        "local",
                        DataNode::local(node_id, file_id, None, None, &text, range),
                    )
                }
            };
            let _ = kind_str;
            (Some(dn), None)
        }
        "df.assign_value" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = crate::languages::shared::find_call_expression(node, &["call"])
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
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => {
            // The captured node is the `identifier` child of a `call` node.
            // Walk up to the parent `call` node and check for a `receiver`
            // to build a qualified name (e.g. "File.open").
            let terminal_text = node_text(node, source).unwrap_or_default();
            let (name, access_path) = node
                .parent()
                .filter(|p| p.kind() == "call")
                .and_then(|call_node| {
                    // Find the receiver text from the call node
                    let mut cursor = call_node.walk();
                    let receiver_text = call_node
                        .named_children(&mut cursor)
                        .find(|c| {
                            c.kind() == "constant"
                                || c.kind() == "identifier"
                                || c.kind() == "instance_variable"
                                || c.kind() == "class_variable"
                                || c.kind() == "global_variable"
                        })
                        .and_then(|r| node_text(r, source));
                    receiver_text.map(|recv| {
                        let qualified = format!("{recv}.{terminal_text}");
                        (qualified.clone(), qualified)
                    })
                })
                .unwrap_or_else(|| {
                    let t = terminal_text.clone();
                    (t.clone(), t)
                });
            let callsite_id = crate::languages::shared::find_call_expression(node, &["call"])
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
        }
        "df.call_arg" => {
            let text = node_text(node, source).unwrap_or_default();
            let callsite_id = crate::languages::shared::find_call_expression(node, &["call"])
                .map(|ce| types::ids::CallsiteId::from_file_byte(&file_id, ce.start_byte() as u32));
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
        "df.field_name" => {
            // Build qualified access_path from receiver.method like df.call_target
            let terminal_text = node_text(node, source).unwrap_or_default();
            let (name, access_path) = node
                .parent()
                .filter(|p| p.kind() == "call")
                .and_then(|call_node| {
                    let mut cursor = call_node.walk();
                    let receiver_text = call_node
                        .named_children(&mut cursor)
                        .find(|c| {
                            c.kind() == "constant"
                                || c.kind() == "identifier"
                                || c.kind() == "instance_variable"
                                || c.kind() == "class_variable"
                                || c.kind() == "global_variable"
                        })
                        .and_then(|r| node_text(r, source));
                    receiver_text.map(|recv| {
                        let qualified = format!("{recv}.{terminal_text}");
                        (qualified.clone(), qualified)
                    })
                })
                .unwrap_or_else(|| {
                    let t = terminal_text.clone();
                    (t.clone(), t)
                });
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
        // ── Ruby dataflow additions (§2.12) ──────────────────────
        "df.implicit_return" => {
            // Query uses trailing `.` anchor: only the last child of
            // body_statement is captured, representing the implicit return.
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
        "df.identifier_use" => {
            if crate::languages::shared::is_identifier_decl_or_property(
                node,
                &["class", "module", "method"],
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
        "df.assign_field_target" => {
            let text = node_text(node, source).unwrap_or_default();
            make_df_assign_field_target(file_id, &text, range)
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let spec = RubyAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = RubyAdapter;
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
        let spec = RubyAdapter;
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
        let spec = RubyAdapter;
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
        let spec = RubyAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = RubyAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow_builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_normalize_ruby() {
        let frontend = ruby_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source =
            "def f(params)\n  name = params[:name]\n  clean = sanitize(name)\n  clean\nend\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("test.rb");
        let ctx = NormalizeCtx {
            language: Language::Ruby,
            file_id,
            file_path: std::path::Path::new("test.rb"),
            source,
        };

        let mut has_parameter = false;
        let mut has_local = false;
        let mut has_field = false;
        let mut has_call_target = false;
        let mut has_implicit_return = false;
        let mut has_expr = false;
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
                    DataNodeKind::Parameter => has_parameter = true,
                    DataNodeKind::Local => has_local = true,
                    DataNodeKind::Field => has_field = true,
                    DataNodeKind::CallTarget => has_call_target = true,
                    DataNodeKind::Return => has_implicit_return = true,
                    DataNodeKind::Expr => has_expr = true,
                    _ => {}
                }
            }
        }
        assert!(has_parameter, "should have Parameter DataNode for params");
        assert!(has_local, "should have Local DataNode for name/clean");
        assert!(has_field, "should have Field DataNode for params[:name]");
        assert!(
            has_call_target,
            "should have CallTarget DataNode for sanitize"
        );
        assert!(
            has_implicit_return,
            "should have Return DataNode for implicit return (clean)"
        );
        assert!(has_expr, "should have Expr DataNode for assignment values");
    }
}
