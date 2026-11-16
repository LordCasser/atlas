//! C++ frontend spec (slot-based).
//!
//! Provides query-driven extraction for C++ source files.

mod callables;
pub(crate) mod dataflow;
pub(crate) mod declarations;
pub(crate) mod lambdas;
pub(crate) mod templates;

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::{
    SymbolDefBuilder, callable_declaration_identity, compact_signature,
    find_c_like_declaration_header, leading_parenthesized, make_binding_def,
    make_df_assign_field_target, make_df_assign_target, make_df_assign_value, make_df_call_arg,
    make_df_call_result, make_df_parameter, make_df_receiver_or_literal, make_df_return_value,
    make_reference_use, make_scope_def,
};
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// C++ frontend spec.
pub(crate) struct CppAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_cpp_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    if capture_name == "definition.lambda" {
        return lambdas::definition(node, source, file_id);
    }
    let mut kind = cpp_definition_kind(capture_name)?;
    if kind == SymbolKind::Function && node.kind() != "qualified_identifier" {
        let declaration = std::iter::successors(node.parent(), |n| n.parent()).find(|n| {
            matches!(
                n.kind(),
                "function_definition" | "declaration" | "field_declaration"
            )
        });
        let owner = declaration.and_then(|declaration| {
            std::iter::successors(declaration.parent(), |n| n.parent()).find(|n| {
                matches!(
                    n.kind(),
                    "class_specifier"
                        | "struct_specifier"
                        | "namespace_definition"
                        | "translation_unit"
                        | "friend_declaration"
                        | "function_definition"
                        | "lambda_expression"
                )
            })
        });
        if owner.is_some_and(|n| matches!(n.kind(), "class_specifier" | "struct_specifier")) {
            // A direct class declaration is a method even when the grammar
            // spells a template member's declarator as an ordinary identifier.
            kind = SymbolKind::Method;
        }
    }
    let node = if kind == SymbolKind::TypeAlias {
        cpp_alias_name(node, source)?
    } else {
        node
    };
    let (written_name, name_node) = if node.kind() == "qualified_identifier" {
        plain_qualified_definition(node, source)?
    } else if node.kind() == "destructor_name" {
        (destructor_name(node, source)?, node)
    } else if node.kind() == "operator_name" {
        // A free/friend operator is not a class member. Friend lookup itself
        // still needs namespace/ADL semantics before it can resolve calls.
        let owner = std::iter::successors(node.parent(), |node| node.parent()).find(|node| {
            matches!(
                node.kind(),
                "friend_declaration"
                    | "class_specifier"
                    | "struct_specifier"
                    | "namespace_definition"
                    | "translation_unit"
            )
        });
        if owner.is_none_or(|owner| !matches!(owner.kind(), "class_specifier" | "struct_specifier"))
        {
            kind = SymbolKind::Function;
        }
        (operator_name(node, source)?, node)
    } else {
        (node_text(node, source)?, node)
    };
    let name = match name_node.kind() {
        "operator_name" => operator_name(name_node, source)?,
        "destructor_name" => destructor_name(name_node, source)?,
        _ => node_text(name_node, source)?,
    };
    let range = node_range(name_node);

    let qualified_name = match written_name.strip_prefix("::") {
        Some(absolute) => absolute.to_string(),
        None => qualified_name_from_node_cpp(&written_name, node, source),
    };
    let signature = cpp_extract_signature(capture_name, node, source);

    let mut symbol =
        SymbolDefBuilder::new(file_id, Language::Cpp, kind, name, qualified_name, range)
            .signature(signature)
            .discriminator(
                if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
                    callable_declaration_identity(node, source)
                } else {
                    None
                },
            )
            .build();
    if matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Field
    ) {
        symbol.static_ = std::iter::successors(name_node.parent(), |node| node.parent())
            .find(|node| {
                matches!(
                    node.kind(),
                    "field_declaration" | "declaration" | "function_definition"
                )
            })
            .is_some_and(|declaration| {
                let mut cursor = declaration.walk();
                declaration.named_children(&mut cursor).any(|child| {
                    child.kind() == "storage_class_specifier"
                        && node_text(child, source).as_deref() == Some("static")
                })
            });
    }
    Some(symbol)
}

/// Preserve separate source occurrences before scopes, bindings and type facts
/// consume symbol identities. A lexical declarator does not uniquely identify
/// conditional definitions, template overloads or repeated declarations.
/// Keep the first occurrence's lexical identity; distinguish later occurrences
/// by source order, so whitespace/body edits alone do not change identities.
/// This does not establish semantic equivalence or select a build branch.
pub(crate) fn distinguish_callable_occurrences(symbols: &mut [SymbolDef]) {
    let mut order: Vec<_> = symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|(index, s)| (s.name_range.start_byte, s.name_range.end_byte, index))
        .collect();
    order.sort_unstable();
    let mut occurrences = std::collections::HashMap::new();
    for (start, end, index) in order {
        let symbol = &mut symbols[index];
        let original = symbol.id;
        let (last_range, ordinal) = occurrences.entry(original).or_insert(((start, end), 0u32));
        if *last_range != (start, end) {
            *ordinal += 1;
            *last_range = (start, end);
        }
        if *ordinal != 0 {
            symbol.id = SymbolId::generate(
                &symbol.file_id,
                symbol.language.as_str(),
                &symbol.qualified_name,
                symbol.kind.as_str(),
                Some(&format!("occurrence:{}:{ordinal}", original.to_hex())),
            );
        }
    }
}

fn destructor_name(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let tokens: Vec<_> = node
        .children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .collect();
    let [tilde, identifier] = tokens.as_slice() else {
        return None;
    };
    (tilde.kind() == "~"
        && !tilde.is_missing()
        && identifier.kind() == "identifier"
        && !identifier.is_missing())
    .then(|| node_text(*identifier, source).map(|name| format!("~{name}")))
    .flatten()
}

fn operator_name(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let tokens: Option<Vec<_>> = node
        .children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .map(|child| node_text(child, source))
        .collect();
    let tokens = tokens?;
    if matches!(
        tokens.get(1).map(String::as_str),
        Some("new" | "delete" | "co_await")
    ) {
        Some(format!("operator {}", tokens[1..].join("")))
    } else {
        Some(tokens.join(""))
    }
}

/// The nearest declaration must itself have a body. A containing class or
/// function is not the definition of a forward declaration/prototype inside it.
pub(crate) fn definition_range(
    root: tree_sitter::Node<'_>,
    symbol: &SymbolDef,
) -> Option<TextRange> {
    if let Some(lambda) = lambdas::node(root, symbol) {
        return Some(node_range(lambda));
    }
    let expected = match symbol.kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
            &["function_definition"][..]
        }
        // The frontend currently represents both class and struct as Class.
        SymbolKind::Class | SymbolKind::Struct => &["class_specifier", "struct_specifier"][..],
        SymbolKind::Enum => &["enum_specifier"][..],
        _ => return None,
    };
    let name = root.descendant_for_byte_range(
        symbol.name_range.start_byte as usize,
        symbol.name_range.end_byte as usize,
    )?;
    let declaration = std::iter::successors(Some(name), |node| node.parent()).find(|node| {
        matches!(
            node.kind(),
            "function_definition"
                | "class_specifier"
                | "struct_specifier"
                | "enum_specifier"
                | "declaration"
                | "field_declaration"
                | "parameter_declaration"
        )
    })?;
    (expected.contains(&declaration.kind()) && declaration.child_by_field_name("body").is_some())
        .then(|| node_range(declaration))
}

fn cpp_alias_name<'tree>(
    mut node: tree_sitter::Node<'tree>,
    source: &str,
) -> Option<tree_sitter::Node<'tree>> {
    loop {
        // In a typedef declarator, the grammar also uses primitive_type for
        // library-defined type names. Their declaration role establishes the
        // name; the node category does not make them language keywords.
        if node.kind() == "primitive_type" {
            let name = node_text(node, source)?;
            return (!name.is_empty()
                && !matches!(
                    name.as_str(),
                    "bool"
                        | "char"
                        | "wchar_t"
                        | "char8_t"
                        | "char16_t"
                        | "char32_t"
                        | "int"
                        | "float"
                        | "double"
                        | "void"
                        | "signed"
                        | "unsigned"
                        | "short"
                        | "long"
                ))
            .then_some(node);
        }
        if matches!(
            node.kind(),
            "type_identifier" | "identifier" | "namespace_identifier"
        ) {
            return Some(node);
        }
        node = if let Some(declarator) = node.child_by_field_name("declarator") {
            declarator
        } else {
            let mut cursor = node.walk();
            let children: Vec<_> = node
                .named_children(&mut cursor)
                .filter(|child| child.kind() != "type_qualifier")
                .collect();
            let [child] = children.as_slice() else {
                return None;
            };
            *child
        };
    }
}

/// Preserve plain nested qualifiers while keeping the simple name's exact range.
/// Templates, dependent names and operators need separate identity handling.
fn plain_qualified_definition<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &str,
) -> Option<(String, tree_sitter::Node<'tree>)> {
    // Recovery can insert a missing `::` between an annotation and a name.
    // Only a written separator establishes this part of a qualified identity.
    let mut cursor = node.walk();
    if !node
        .children(&mut cursor)
        .any(|child| child.kind() == "::" && !child.is_missing())
    {
        return None;
    }
    let prefix = match node.child_by_field_name("scope") {
        Some(scope) if scope.kind() == "namespace_identifier" => node_text(scope, source)?,
        Some(_) => return None,
        None => String::new(), // Leading global :: qualification.
    };
    let name = node.child_by_field_name("name")?;
    let (suffix, name_node) = match name.kind() {
        "qualified_identifier" => plain_qualified_definition(name, source)?,
        "identifier" | "field_identifier" | "type_identifier" => (node_text(name, source)?, name),
        "operator_name" => (operator_name(name, source)?, name),
        "destructor_name" => (destructor_name(name, source)?, name),
        _ => return None,
    };
    Some((format!("{prefix}::{suffix}"), name_node))
}

/// The grammar represents built-in functional casts and named casts as calls.
/// Share this syntactic distinction between reference and value extraction;
/// implicit conversion effects require their own semantics.
fn is_conversion_callee(node: tree_sitter::Node, source: &str) -> bool {
    node.kind() == "primitive_type"
        || (node.kind() == "template_function"
            && node.child_by_field_name("name").is_some_and(|name| {
                matches!(
                    name.utf8_text(source.as_bytes()).ok(),
                    Some("static_cast" | "dynamic_cast" | "const_cast" | "reinterpret_cast")
                )
            }))
}

/// GNU attribute-list entries name attributes, not invoked functions. The
/// grammar uses call_expression for entries with parameters. Expressions
/// inside those parameters are separate nodes and must remain available.
fn is_attribute_call(call: tree_sitter::Node<'_>, source: &str) -> bool {
    if call.kind() != "call_expression" {
        return false;
    }
    let Some(parent) = call.parent() else {
        return false;
    };
    if parent.kind() == "argument_list"
        && parent
            .parent()
            .is_some_and(|n| n.kind() == "attribute_specifier")
    {
        return true;
    }

    // Some declaration placements (including GNU-attributed namespaces) are
    // not recognized by the pinned grammar. Match the explicit introducer and
    // double-parenthesized attribute list, independently of ERROR node shapes.
    let mut item = call;
    while let Some(parent) = item.parent().filter(|n| n.kind() == "comma_expression") {
        item = parent;
    }
    let Some(inner) = item
        .parent()
        .filter(|n| n.kind() == "parenthesized_expression")
    else {
        return false;
    };
    let Some(outer) = inner
        .parent()
        .filter(|n| n.kind() == "parenthesized_expression")
    else {
        return false;
    };
    let mut preceding = outer;
    loop {
        // Recovery may split the introducer and list across adjacent nodes.
        // Walk to the preceding leaf token, skipping comments and missing nodes.
        while preceding.prev_sibling().is_none() {
            let Some(parent) = preceding.parent() else {
                return false;
            };
            preceding = parent;
        }
        preceding = preceding.prev_sibling().unwrap();
        if preceding.kind() == "comment" || preceding.is_missing() {
            continue;
        }
        while let Some(child) = preceding
            .child_count()
            .checked_sub(1)
            .and_then(|i| u32::try_from(i).ok())
            .and_then(|i| preceding.child(i))
        {
            preceding = child;
        }
        if preceding.kind() == "comment" || preceding.is_missing() {
            continue;
        }
        return matches!(
            preceding.utf8_text(source.as_bytes()).ok(),
            Some("__attribute__" | "__attribute")
        );
    }
}

fn normalize_cpp_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    if capture_name == "reference.expression_call" {
        let call = node.parent()?;
        if is_attribute_call(call, source) {
            return None;
        }
        let args = call.child_by_field_name("arguments")?;
        let mut cursor = call.walk();
        let outside_function = call.children(&mut cursor).any(|child| {
            child != node && child.start_byte() < args.start_byte() && child.kind() != "comment"
        });
        if outside_function {
            // A function field can be only a fragment of the written callee
            // after parser recovery. Preserve the entire prefix of the call,
            // without binding it to that fragment or interpreting extra tokens.
            let mut range = node_range(call);
            range.end_byte = args.start_byte() as u32;
            range.end_line = args.start_position().row as u32;
            range.end_column = args.start_position().column as u32;
            let text = source
                .get(call.start_byte()..args.start_byte())?
                .to_string();
            let mut reference =
                make_reference_use(file_id, ReferenceKind::Call, text.clone(), text, range);
            reference.arity = call_arity(call);
            return Some(reference);
        }
        if is_conversion_callee(node, source) {
            // The grammar also represents conversion syntax as a call.
            // Conversion may invoke user code, but it is not an explicit call
            // to a function named int/static_cast/etc. Its implicit targets
            // need conversion semantics. Calls inside its operand are retained
            // by their own captures, as is an invocation of the result.
            return None;
        }
        if let Some(reference) = lambdas::reference(node, source, file_id) {
            return Some(reference);
        }
        if let Some(name) = callee_name(node) {
            return normalize_cpp_reference("reference.call", name, source, file_id);
        }
        // The callee may itself contain calls, e.g. factory()() or
        // (event->callback())(). Keep its own identity and written expression;
        // a nested name is not the target of this invocation.
        let text = node_text(node, source)?;
        let mut reference = make_reference_use(
            file_id,
            ReferenceKind::Call,
            text.clone(),
            text,
            node_range(node),
        );
        reference.arity = call_arity(node.parent()?);
        return Some(reference);
    }
    let kind = cpp_reference_kind(capture_name)?;
    // Captured node is the simple name (identifier / field_identifier).
    let name = node_text(node, source)?;
    let range = node_range(node);

    // Walk to the outermost qualified_identifier so nested A::B::C keeps full text.
    let (text, receiver) = if let Some(field) = node
        .parent()
        .filter(|p| p.kind() == "field_expression" && p.child_by_field_name("field") == Some(node))
    {
        (
            node_text(field, source)?,
            field
                .child_by_field_name("argument")
                .and_then(|n| node_text(n, source)),
        )
    } else {
        qualified_call_text_and_receiver(node, source, &name)
    };

    // source_symbol is resolved by SemanticBinder after extraction.
    let mut r = make_reference_use(file_id, kind, text, name, range);
    if let Some(recv) = receiver {
        r.receiver = Some(recv);
    }
    if kind == ReferenceKind::Call {
        let mut callee = node;
        while let Some(parent) = callee.parent() {
            if parent.kind() == "call_expression" {
                if parent.child_by_field_name("function") == Some(callee)
                    && let Some(args) = parent.child_by_field_name("arguments")
                    && !args.has_error()
                {
                    r.arity = call_arity(parent);
                }
                break;
            }
            if !matches!(parent.kind(), "qualified_identifier" | "field_expression") {
                break;
            }
            callee = parent;
        }
    }
    Some(r)
}

/// A raw callee containing a statement separator outside balanced delimiters
/// does not establish one expression. Recovery can join an initializer and
/// the following statement. Nested lambda/statement-expression bodies, quoted
/// literals and comments must not be mistaken for such separators. Unknown
/// tokenization is not evidence for discarding the observation.
fn call_crosses_statement(call: tree_sitter::Node<'_>, source: &str) -> bool {
    let Some(args) = call.child_by_field_name("arguments") else {
        return false;
    };
    let Some(prefix) = source.get(call.start_byte()..args.start_byte()) else {
        return false;
    };
    if !prefix.contains(';') {
        return false;
    }
    let Some(tokens) = crate::cpp_member_macros::tokens(prefix) else {
        return false;
    };
    let mut delimiters = Vec::new();
    let mut separator = false;
    for token in &tokens {
        match token.as_str() {
            "(" => delimiters.push(")"),
            "[" => delimiters.push("]"),
            "{" => delimiters.push("}"),
            ")" | "]" | "}" => {
                if delimiters.pop() != Some(token.as_str()) {
                    return false;
                }
            }
            ";" if delimiters.is_empty() => separator = true,
            _ => {}
        }
    }
    separator && delimiters.is_empty()
}

pub(crate) fn discard_statement_crossing_calls(
    root: tree_sitter::Node<'_>,
    source: &str,
    references: &mut Vec<ReferenceUse>,
    diagnostics: &mut Vec<ExtractDiagnostic>,
    token: &dyn crate::cancel::CancelCheck,
) -> bool {
    let mut cancelled = false;
    references.retain(|reference| {
        if token.is_cancelled() {
            cancelled = true;
            return true;
        }
        if reference.kind != ReferenceKind::Call || !reference.text.contains(';') {
            return true;
        }
        let call = root
            .descendant_for_byte_range(
                reference.range.start_byte as usize,
                reference.range.end_byte as usize,
            )
            .and_then(|node| {
                std::iter::successors(Some(node), |node| node.parent())
                    .find(|node| node.kind() == "call_expression")
            });
        if let Some(call) = call.filter(|call| call_crosses_statement(*call, source)) {
            diagnostics.push(ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: "Recovered callee crosses a statement boundary; inspect this source region or its macro expansion before treating it as a call".into(),
                range: Some(node_range(call)),
            });
            return false;
        }
        true
    });
    !cancelled
}

fn callee_name(mut node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    loop {
        node = match node.kind() {
            "identifier" | "field_identifier" | "type_identifier" => return Some(node),
            "qualified_identifier" => node.child_by_field_name("name")?,
            "field_expression" => node
                .child_by_field_name("field")
                .filter(|field| field.kind() == "field_identifier")?,
            // Parentheses can change lookup (including ADL); do not erase
            // them or infer the identity of a returned callable here.
            _ => return None,
        };
    }
}

fn call_arity(call: tree_sitter::Node<'_>) -> Option<u32> {
    let args = call.child_by_field_name("arguments")?;
    if args.has_error() {
        return None;
    }
    let mut cursor = args.walk();
    let args: Vec<_> = args
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect();
    if args.iter().any(|n| n.kind() == "parameter_pack_expansion") {
        return None;
    }
    u32::try_from(args.len()).ok()
}

/// For `A::B::C`, tree-sitter nests `qualified_identifier` nodes. Walk to the
/// outermost ancestor so `text` is the full span and `receiver` is the prefix
/// before the last `::` segment.
fn qualified_call_text_and_receiver(
    name_node: tree_sitter::Node,
    source: &str,
    simple_name: &str,
) -> (String, Option<String>) {
    let mut outermost: Option<tree_sitter::Node> = None;
    let mut cur = name_node.parent();
    while let Some(p) = cur {
        if p.kind() == "qualified_identifier" {
            outermost = Some(p);
            cur = p.parent();
        } else {
            break;
        }
    }
    let Some(qi) = outermost else {
        return (simple_name.to_string(), None);
    };
    let full = node_text(qi, source).unwrap_or_else(|| simple_name.to_string());
    let receiver = full
        .rsplit_once("::")
        .map(|(prefix, _)| prefix.to_string())
        .filter(|p| !p.is_empty());
    (full, receiver)
}

fn normalize_cpp_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = cpp_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let is_relative = !module.starts_with('<');

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
        local_name: None,
        is_wildcard: false,
        is_relative,
        range,
        alias: None,
    })
}

fn normalize_cpp_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = cpp_scope_kind(capture_name)?;
    let name = node_text(node, source).unwrap_or_default();
    let range = node_range(node);

    Some(make_scope_def(file_id, kind, name, String::new(), range))
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for CppAdapter {
    fn language(&self) -> Language {
        Language::Cpp
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cpp::LANGUAGE.into()
    }
    fn refine_tree(
        &self,
        source: &str,
        tree: tree_sitter::Tree,
        canceled: &dyn Fn() -> bool,
    ) -> Option<tree_sitter::Tree> {
        crate::cpp_initialization::refine(source, tree, canceled)
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for CppAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/cpp/definitions.scm")
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/cpp/manifest.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_cpp_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for CppAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/cpp/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_cpp_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for CppAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/cpp/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_cpp_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for CppAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/cpp/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_cpp_scope(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl LexicalBindingSpec for CppAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/cpp/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.70,
            vec!["scope-chain-aware binding with shadowing support"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_cpp_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for CppAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/cpp/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.70,
            vec![
                "AST-driven local dataflow; direct-identifier compound/update expressions preserve aggregate read-modify-write provenance (0.90), and compound RHS-only writes are suppressed; field/subscript/pointer mutation targets, overloaded-operator dispatch/conversions, and prefix/postfix result timing remain conservative",
            ],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_cpp_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
    }

    fn build_language_edges(
        &self,
        ctx: &crate::extraction_ctx::ExtractionCtx<'_>,
        pos_map: &std::collections::HashMap<crate::dataflow_builder::NodePosKey, DataNodeId>,
        nodes: &[DataNode],
        _bindings: &[BindingDef],
        _scopes: &[ScopeDef],
        edges: &mut Vec<DataFlowEdge>,
    ) -> anyhow::Result<()> {
        dataflow::field_receivers(ctx, pos_map, nodes, edges);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn cpp_frontend() -> LanguageFrontend {
    let lang = Language::Cpp;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(CppAdapter),
        symbols: Box::new(CppAdapter),
        references: Box::new(CppAdapter),
        imports: Box::new(CppAdapter),
        scopes: Box::new(CppAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(CppAdapter),
        dataflow: Box::new(CppAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn qualified_name_from_node_cpp(name: &str, node: tree_sitter::Node, source: &str) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        match parent.kind() {
            "class_specifier" | "struct_specifier" => {
                if let Some(child) = parent.child_by_field_name("name")
                    && let Some(class_name) = if child.kind() == "qualified_identifier" {
                        plain_qualified_definition(child, source)
                            .map(|(name, _)| name)
                            // Unsupported qualifiers still enclose their members.
                            // Losing the owner would turn them into free functions.
                            .or_else(|| node_text(child, source))
                    } else {
                        node_text(child, source)
                    }
                {
                    // The same spelling normalization applies to the class and
                    // its members. Comments are not part of a qualified identity;
                    // an absolute class owner already supplies the full scope.
                    if let Some(absolute) = class_name.strip_prefix("::") {
                        parts.push(absolute.to_string());
                        break;
                    }
                    parts.push(class_name);
                }
            }
            "namespace_definition" => {
                if let Some(child) = parent.child_by_field_name("name")
                    && let Ok(ns_name) = child.utf8_text(source.as_bytes())
                {
                    parts.push(ns_name.to_string());
                }
            }
            _ => {}
        }
        current = parent;
    }

    parts.reverse();
    parts.join("::")
}

fn cpp_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.type_alias" => Some(SymbolKind::TypeAlias),
        "definition.function" | "definition.function_declaration" => Some(SymbolKind::Function),
        "definition.method" => Some(SymbolKind::Method),
        "definition.class" => Some(SymbolKind::Class),
        "definition.namespace" => Some(SymbolKind::Namespace),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.field" => Some(SymbolKind::Field),
        "definition.macro" => Some(SymbolKind::Macro),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

fn cpp_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.type" => Some(ReferenceKind::TypeReference),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        _ => None,
    }
}

fn cpp_scope_kind(capture: &str) -> Option<ScopeKind> {
    match capture {
        "scope.file" => Some(ScopeKind::File),
        "scope.function" => Some(ScopeKind::Function),
        "scope.class" => Some(ScopeKind::Class),
        "scope.enum" => Some(ScopeKind::Enum),
        "scope.namespace" => Some(ScopeKind::Namespace),
        "scope.block" => Some(ScopeKind::Block),
        "scope.conditional" => Some(ScopeKind::Conditional),
        "scope.loop" => Some(ScopeKind::Loop),
        _ => None,
    }
}

fn cpp_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned, String::new()))
        }
        "import.include" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned, String::new()))
        }
        "import.name" => {
            let name = node_text(node, source)?;
            Some((ImportKind::Use, String::new(), name))
        }
        _ => None,
    }
}

fn cpp_extract_signature(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    if !matches!(
        capture_name,
        "definition.function" | "definition.function_declaration" | "definition.method"
    ) {
        return None;
    }
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == "template_declaration" {
            // Template substitution is outside the plain callable-signature subset.
            return None;
        }
        if matches!(parent.kind(), "namespace_definition" | "translation_unit") {
            break;
        }
        ancestor = parent.parent();
    }
    let name = node_text(node, source)?;
    let declaration = find_c_like_declaration_header(node, source)?;
    cpp_signature_from_header(&declaration, &name)
}

fn cpp_signature_from_header(header: &str, name: &str) -> Option<String> {
    let name_pos = header.rfind(name)?;
    let before_name = header[..name_pos].trim();
    let after_name = header[name_pos + name.len()..].trim();
    let params = leading_parenthesized(after_name)?;
    // Preserve cv/ref qualifiers: `f() const` and `f()` are distinct overloads.
    // override/final/pure-specifiers are declaration-only, not type identity.
    let qualifiers = after_name[params.len()..]
        .split('=')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter(|part| !matches!(*part, "override" | "final"))
        .collect::<Vec<_>>()
        .join(" ");
    let callable = if qualifiers.is_empty() {
        params.to_string()
    } else {
        format!("{params} {qualifiers}")
    };
    let return_type = before_name.trim_end_matches(['*', '&']).trim();
    if return_type.is_empty() {
        compact_signature(&callable)
    } else {
        compact_signature(&format!("{callable}: {return_type}"))
    }
}

// ── Lexical binding normalize ──────────────────────────────────────────

fn cpp_declarator_binding_kind(mut node: tree_sitter::Node<'_>) -> Option<BindingKind> {
    loop {
        let parent = node.parent()?;
        match parent.kind() {
            "parameter_declaration" | "optional_parameter_declaration" => {
                return Some(BindingKind::Parameter);
            }
            "declaration" => return Some(BindingKind::Local),
            "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator"
            | "init_declarator"
            | "structured_binding_declarator" => {}
            // A pointer-to-function variable is a binding. Ordinary function
            // declarations are handled by symbol/name lookup instead.
            "function_declarator"
                if matches!(
                    node.kind(),
                    "parenthesized_declarator" | "pointer_declarator"
                ) || std::iter::successors(parent.parent(), |node| node.parent())
                    .any(|node| node.kind() == "function_definition") => {}
            _ => return None,
        }
        if let Some(declarator) = parent.child_by_field_name("declarator") {
            if declarator.id() != node.id() {
                return None;
            }
        }
        node = parent;
    }
}

fn normalize_cpp_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<BindingDef> {
    if capture_name != "lexical.declarator" {
        return None;
    }
    let kind = cpp_declarator_binding_kind(node)?;
    let name = node_text(node, source)?;
    let range = node_range(node);
    Some(make_binding_def(file_id, kind, name, range))
}

/// A named parameter of this callable, with its argument slot when established.
/// A lexical parameter remains a local input when a partial list or an earlier
/// pack prevents invocation mapping. Nested function types and local prototypes
/// do not declare parameters of the enclosing function.
fn cpp_dataflow_parameter_slot(node: tree_sitter::Node<'_>) -> Option<Option<u32>> {
    if cpp_declarator_binding_kind(node) != Some(BindingKind::Parameter) {
        return None;
    }
    let parameter = std::iter::successors(node.parent(), |node| node.parent()).find(|node| {
        matches!(
            node.kind(),
            "parameter_declaration" | "optional_parameter_declaration"
        )
    })?;
    // Only the declared name is an entry input. An identifier in a default
    // expression can have the same parameter parent but is an evaluated use.
    let declared = parameter.child_by_field_name("declarator")?;
    if node.start_byte() < declared.start_byte() || node.end_byte() > declared.end_byte() {
        return None;
    }
    let parameters = parameter
        .parent()
        .filter(|node| node.kind() == "parameter_list")?;
    let mut declarator = parameters.parent().filter(|node| {
        matches!(
            node.kind(),
            "function_declarator" | "abstract_function_declarator"
        )
    })?;
    loop {
        let parent = declarator.parent()?;
        match parent.kind() {
            "function_definition" | "lambda_expression"
                if parent.child_by_field_name("declarator") == Some(declarator) =>
            {
                break;
            }
            "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
                if parent
                    .child_by_field_name("declarator")
                    .is_some_and(|child| child != declarator)
                {
                    return None;
                }
            }
            _ => return None,
        }
        declarator = parent;
    }
    if parameters.has_error() {
        return Some(None);
    }
    let mut index = 0;
    let mut cursor = parameters.walk();
    for sibling in parameters.named_children(&mut cursor) {
        if sibling.kind() == "comment" {
            continue;
        }
        // Packs and explicit object parameters require invocation-specific
        // mapping. Do not shift later positions across an unsupported slot.
        if !matches!(
            sibling.kind(),
            "parameter_declaration" | "optional_parameter_declaration"
        ) {
            return Some(None);
        }
        let mut cursor = sibling.walk();
        if sibling
            .children(&mut cursor)
            .any(|child| child.kind() == "this")
        {
            return Some(None);
        }
        if sibling == parameter {
            return Some(Some(index));
        }
        index += 1;
    }
    None
}

// ── Dataflow normalize ─────────────────────────────────────────────────

fn normalize_cpp_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    use types::ids::DataNodeId;
    if capture_name == "df.call_arg"
        && node
            .parent()
            .and_then(|args| args.parent())
            .is_some_and(|parent| matches!(parent.kind(), "attribute_specifier" | "attribute"))
    {
        // Both GNU attribute lists and standard attribute parameters use an
        // argument_list in the grammar, without being invocation arguments.
        return (None, None);
    }
    if capture_name == "df.call_result"
        && node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|callee| is_conversion_callee(callee, source))
    {
        return (None, None);
    }
    if matches!(
        capture_name,
        "df.call_target" | "df.call_arg" | "df.call_result"
    ) {
        let call = if capture_name == "df.call_result" {
            Some(node)
        } else if capture_name == "df.call_target" {
            node.parent().filter(|parent| {
                parent.kind() == "call_expression"
                    && parent.child_by_field_name("function") == Some(node)
            })
        } else {
            node.parent().and_then(|parent| {
                crate::languages::shared::find_call_expression(
                    parent,
                    &["call_expression", "new_expression"],
                )
            })
        }
        .or_else(|| {
            crate::languages::shared::find_call_expression(
                node,
                &["call_expression", "new_expression"],
            )
        });
        if call.is_some_and(|call| {
            is_attribute_call(call, source) || call_crosses_statement(call, source)
        }) {
            return (None, None);
        }
    }
    let range = node_range(node);
    match capture_name {
        "df.call_result" => make_df_call_result(file_id, node, source, range),
        "df.parameter" => {
            let Some(index) = cpp_dataflow_parameter_slot(node) else {
                return (None, None);
            };
            let (mut parameter, edge) = make_df_parameter(file_id, node, source, range);
            if let Some(parameter) = &mut parameter {
                parameter.arg_index = index;
            }
            (parameter, edge)
        }
        "df.assign_target" | "df.mutation_target" => {
            if capture_name == "df.assign_target"
                && cpp_declarator_binding_kind(node) == Some(BindingKind::Parameter)
            {
                return (None, None);
            }
            make_df_assign_target(file_id, node, source, range)
        }
        "df.assign_value" | "df.mutation_value" => make_df_assign_value(
            file_id,
            node,
            source,
            range,
            &["call_expression", "new_expression"],
        ),
        "df.return_value" => make_df_return_value(file_id, node, source, range),
        "df.call_target" => node_text(node, source)
            .map(|name| {
                let access_path = name.clone();
                let callsite_id = node
                    .parent()
                    .filter(|parent| {
                        parent.kind() == "call_expression"
                            && parent.child_by_field_name("function") == Some(node)
                    })
                    .or_else(|| {
                        crate::languages::shared::find_call_expression(
                            node,
                            &["call_expression", "new_expression"],
                        )
                    })
                    .map(|ce| {
                        types::ids::CallsiteId::from_file_range(
                            &file_id,
                            ce.start_byte() as u32,
                            ce.end_byte() as u32,
                        )
                    });
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
        "df.call_arg" => make_df_call_arg(
            file_id,
            node,
            source,
            range,
            &["call_expression", "new_expression"],
        ),
        "df.field_name" => node_text(node, source)
            .map(|name| {
                let access_path = node
                    .parent()
                    .filter(|p| p.kind() == "field_expression")
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
            make_df_receiver_or_literal(file_id, capture_name, node, source, range)
        }
        "df.identifier_use" | "df.mutation_read" => {
            if capture_name == "df.identifier_use"
                && (cpp_dataflow_parameter_slot(node).is_some()
                    || crate::languages::shared::is_identifier_decl_or_property(
                        node,
                        &["template_declaration", "type_definition"],
                    ))
            {
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
        let spec = CppAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = CppAdapter;
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
        let spec = CppAdapter;
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
        let spec = CppAdapter;
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
        let spec = CppAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_dataflow_builder_query_parses() {
        let spec = CppAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.dataflow_builder_query());
        assert!(
            query.is_ok(),
            "dataflow builder query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_dataflow_reference_and_new_expression() {
        let frontend = super::cpp_frontend();
        let ts_lang = frontend.parser.tree_sitter_language();
        let source = "void f() { int x = 0; int& ref = x; auto p = new Foo(1, 2); }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let query =
            tree_sitter::Query::new(&ts_lang, frontend.dataflow.dataflow_builder_query()).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let file_id = FileId::generate("Test.cpp");
        let ctx = NormalizeCtx {
            language: Language::Cpp,
            file_id,
            file_path: std::path::Path::new("Test.cpp"),
            source,
        };

        let mut node_hits = 0;
        let mut has_local = false;
        let mut has_call_target = false;
        let mut has_call_arg = false;
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
                node_hits += 1;
                match dn.kind {
                    DataNodeKind::Local => {
                        // Check for the reference binding "ref"
                        if dn.name.as_deref() == Some("ref") {
                            has_local = true;
                        }
                    }
                    DataNodeKind::CallTarget => {
                        if dn.name.as_deref() == Some("Foo") {
                            has_call_target = true;
                        }
                    }
                    DataNodeKind::CallArg => has_call_arg = true,
                    _ => {}
                }
            }
        }
        assert!(
            node_hits > 0,
            "dataflow query should produce DataNodes for ref binding + new expression"
        );
        assert!(has_local, "should have a local DataNode from int& ref = x");
        assert!(
            has_call_target,
            "should have a CallTarget DataNode from new Foo(1, 2)"
        );
        assert!(
            has_call_arg,
            "should have CallArg DataNodes from new Foo(1, 2)"
        );
    }
}
