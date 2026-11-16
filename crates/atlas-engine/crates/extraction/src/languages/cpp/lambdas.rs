//! Closure identity, construction context and written invocation sites.
use std::collections::HashMap;

use tree_sitter::Node;
use types::{
    BindingId, FileFacts, FileId, ReferenceKind, ReferenceUse, SymbolDef, SymbolId, SymbolKind,
    TextRange,
};

use crate::languages::shared::{SymbolDefBuilder, make_reference_use};
use crate::languages::{node_range, node_text};
use crate::symbol_registry::SymbolRegistry;

fn name(lambda: Node<'_>) -> String {
    format!("<lambda@{}>", lambda.start_byte())
}

pub(super) fn definition(lambda: Node<'_>, source: &str, file: FileId) -> Option<SymbolDef> {
    let captures = lambda.child_by_field_name("captures")?;
    lambda.child_by_field_name("body")?;
    let name = name(lambda);
    // Preserve the lexical class/namespace context, including out-of-class
    // member definitions. A closure does not introduce a named lookup scope.
    let enclosing = std::iter::successors(lambda.parent(), |n| n.parent())
        .find(|n| n.kind() == "function_definition")
        .and_then(|function| {
            let mut declarator = function.child_by_field_name("declarator")?;
            while declarator.kind() != "function_declarator" {
                declarator = declarator.child_by_field_name("declarator")?;
            }
            super::normalize_cpp_definition(
                "definition.function",
                declarator.child_by_field_name("declarator")?,
                source,
                file,
            )
        });
    let qualified = if let Some(enclosing) = enclosing {
        match enclosing.qualified_name.rsplit_once("::") {
            Some((owner, _)) => format!("{owner}::{name}"),
            None => name.clone(),
        }
    } else {
        super::qualified_name_from_node_cpp(&name, captures, source)
    };
    Some(
        SymbolDefBuilder::new(
            file,
            types::Language::Cpp,
            SymbolKind::Function,
            name,
            qualified,
            node_range(captures),
        )
        .build(),
    )
}

pub(crate) fn node<'a>(root: Node<'a>, symbol: &SymbolDef) -> Option<Node<'a>> {
    let captures = root.descendant_for_byte_range(
        symbol.name_range.start_byte as usize,
        symbol.name_range.end_byte as usize,
    )?;
    let lambda = captures.parent()?;
    (captures.kind() == "lambda_capture_specifier" && lambda.kind() == "lambda_expression")
        .then_some(lambda)
}

pub(crate) fn caller_initializers(
    root: Node<'_>,
    symbols: &[SymbolDef],
) -> HashMap<SymbolId, TextRange> {
    symbols
        .iter()
        .filter_map(|symbol| node(root, symbol).map(|_| (symbol.id, symbol.name_range)))
        .collect()
}

fn unwrap_parentheses(mut expression: Node<'_>) -> Option<Node<'_>> {
    while expression.kind() == "parenthesized_expression" {
        let mut cursor = expression.walk();
        let mut children = expression
            .named_children(&mut cursor)
            .filter(|n| n.kind() != "comment");
        expression = children.next()?;
        if children.next().is_some() {
            return None;
        }
    }
    Some(expression)
}

pub(super) fn reference(expression: Node<'_>, source: &str, file: FileId) -> Option<ReferenceUse> {
    let lambda = unwrap_parentheses(expression)?;
    if lambda.kind() != "lambda_expression" {
        return None;
    }
    let captures = lambda.child_by_field_name("captures")?;
    let call = expression.parent()?;
    let mut reference = make_reference_use(
        file,
        ReferenceKind::Call,
        node_text(expression, source)?,
        name(lambda),
        node_range(captures),
    );
    reference.arity = super::call_arity(call);
    Some(reference)
}

pub(super) fn annotate(
    lambda: Node<'_>,
    source: &str,
    facts: &FileFacts,
    registry: &SymbolRegistry,
    capture: &mut types::cpp::CppLambdaCapture,
) {
    let Some(captures) = lambda.child_by_field_name("captures") else {
        return;
    };
    let range = node_range(captures);
    capture.symbol_id = facts
        .symbols
        .iter()
        .find(|s| s.name_range == range && s.kind == SymbolKind::Function)
        .map(|s| s.id);
    capture.enclosing_symbol = registry.source_for_range(range);
    if let Some((binding, const_)) = local_binding(lambda, source, facts) {
        capture.binding_id = Some(binding);
        capture.binding_const = const_;
    }
    let mut cursor = lambda.walk();
    if captures.has_error()
        || lambda
            .named_children(&mut cursor)
            .any(|n| matches!(n.kind(), "template_parameter_list" | "requires_clause"))
    {
        return;
    }
    capture.parameter_count = match lambda.child_by_field_name("declarator") {
        None => Some(0),
        Some(declarator) => {
            if declarator.has_error() {
                return;
            }
            let Some(parameters) = declarator.child_by_field_name("parameters") else {
                return;
            };
            let mut cursor = parameters.walk();
            let parameters: Vec<_> = parameters
                .named_children(&mut cursor)
                .filter(|n| n.kind() != "comment")
                .collect();
            if parameters.iter().any(|n| {
                !matches!(
                    n.kind(),
                    "parameter_declaration" | "optional_parameter_declaration"
                )
            }) {
                return;
            }
            u32::try_from(parameters.len()).ok()
        }
    };
}

fn local_binding(lambda: Node<'_>, source: &str, facts: &FileFacts) -> Option<(BindingId, bool)> {
    let mut expression = lambda;
    while let Some(parent) = expression
        .parent()
        .filter(|p| p.kind() == "parenthesized_expression")
    {
        expression = parent;
    }
    let initializer = expression.parent()?;
    if initializer.kind() != "init_declarator"
        || initializer.child_by_field_name("value") != Some(expression)
    {
        return None;
    }
    let name = initializer.child_by_field_name("declarator")?;
    if name.kind() != "identifier" {
        return None;
    }
    let declaration = initializer.parent()?;
    if declaration.kind() != "declaration"
        || declaration.has_error()
        || node_text(declaration.child_by_field_name("type")?, source)?.trim() != "auto"
    {
        return None;
    }
    let mut cursor = declaration.walk();
    let children: Vec<_> = declaration.named_children(&mut cursor).collect();
    if children
        .iter()
        .filter(|n| n.kind() == "init_declarator")
        .count()
        != 1
    {
        return None;
    }
    let mut const_ = false;
    for qualifier in children.iter().filter(|n| n.kind() == "type_qualifier") {
        if node_text(*qualifier, source)?.trim() != "const" {
            return None;
        }
        const_ = true;
    }
    facts
        .bindings
        .iter()
        .find(|b| b.range == node_range(name))
        .map(|b| (b.id, const_))
}

pub(super) fn scope_supported(root: Node<'_>, symbol: &SymbolDef, facts: &FileFacts) -> bool {
    let Some(lambda) = node(root, symbol) else {
        return false;
    };
    if std::iter::successors(lambda.parent(), |n| n.parent()).any(|n| n.kind() == "ERROR") {
        return false;
    }
    let enclosing = facts
        .symbols
        .iter()
        .filter(|candidate| {
            candidate.id != symbol.id
                && matches!(candidate.kind, SymbolKind::Function | SymbolKind::Method)
                && node(root, candidate).is_none()
                && candidate.range.start_byte <= symbol.range.start_byte
                && candidate.range.end_byte >= symbol.range.end_byte
        })
        .min_by_key(|candidate| candidate.range.byte_len());
    enclosing.is_none_or(|owner| super::callables::scope_supported(root, owner))
}
