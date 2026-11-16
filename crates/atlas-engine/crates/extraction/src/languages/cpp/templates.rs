//! Written function-template syntax shared by indexing and read-only inspection.
//! This module does not perform name lookup, substitution or overload selection.
use tree_sitter::Node;
use types::{ReferenceUse, cpp::CppTemplateCall};

use crate::languages::{node_range, node_text};

fn single_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut children = node
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment");
    let child = children.next()?;
    children.next().is_none().then_some(child)
}

/// Include unsupported primaries and explicit specializations in the name
/// inventory even when their declarator did not produce a callable symbol.
pub(super) fn function_name(template: Node<'_>, source: &str) -> Option<String> {
    if template.kind() != "template_declaration" {
        return None;
    }
    let mut cursor = template.walk();
    let declaration = template.named_children(&mut cursor).find(|n| {
        matches!(
            n.kind(),
            "function_definition" | "declaration" | "field_declaration"
        )
    })?;
    let mut node = declaration.child_by_field_name("declarator")?;
    while matches!(node.kind(), "pointer_declarator" | "reference_declarator") {
        node = node
            .child_by_field_name("declarator")
            .or_else(|| single_child(node))?;
    }
    if node.kind() != "function_declarator" {
        return None;
    }
    fn name(node: Node<'_>, source: &str) -> Option<String> {
        match node.kind() {
            "identifier" | "field_identifier" | "namespace_identifier" | "type_identifier" => {
                node_text(node, source)
            }
            "template_function" | "template_method" => {
                name(node.child_by_field_name("name")?, source)
            }
            "qualified_identifier" => Some(format!(
                "{}::{}",
                match node.child_by_field_name("scope") {
                    Some(scope) => name(scope, source)?,
                    None => String::new(),
                },
                name(node.child_by_field_name("name")?, source)?
            )),
            _ => None,
        }
    }
    name(node.child_by_field_name("declarator")?, source)
}

struct Callee<'tree> {
    name: Node<'tree>,
    arguments: Node<'tree>,
    text: String,
    receiver: Option<String>,
}

fn callee<'tree>(mut node: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    if node.has_error() {
        return None;
    }
    let mut prefix = String::new();
    let mut receiver = None;
    loop {
        node = match node.kind() {
            "template_function" | "template_method" => {
                let name = node.child_by_field_name("name")?;
                if !matches!(name.kind(), "identifier" | "field_identifier") {
                    return None;
                }
                let text = format!("{prefix}{}", node_text(name, source)?);
                if receiver.is_none() {
                    receiver = text.rsplit_once("::").map(|(scope, _)| scope.to_string());
                }
                return Some(Callee {
                    name,
                    arguments: node.child_by_field_name("arguments")?,
                    text,
                    receiver,
                });
            }
            "parenthesized_expression" | "dependent_name" => single_child(node)?,
            "qualified_identifier" => {
                let mut cursor = node.walk();
                if !node
                    .children(&mut cursor)
                    .any(|n| n.kind() == "::" && !n.is_missing())
                {
                    return None;
                }
                if let Some(scope) = node.child_by_field_name("scope") {
                    prefix.push_str(&node_text(scope, source)?);
                }
                prefix.push_str("::");
                node.child_by_field_name("name")?
            }
            "field_expression" => {
                let object = node_text(node.child_by_field_name("argument")?, source)?;
                let operator = node_text(node.child_by_field_name("operator")?, source)?;
                if !matches!(operator.as_str(), "." | "->") {
                    return None;
                }
                prefix.push_str(&object);
                prefix.push_str(&operator);
                if receiver.is_none() {
                    receiver = Some(object);
                }
                node.child_by_field_name("field")?
            }
            // A returned callable is a separate invocation, never the nested name.
            _ => return None,
        };
    }
}

/// Extract the exact written callee of a recorded call. Original reference
/// identity/range stay unchanged; normalized spelling is only for later lookup.
pub fn template_call(
    root: Node<'_>,
    reference: &ReferenceUse,
    source: &str,
) -> Option<CppTemplateCall> {
    if !reference.text.contains('<') {
        return None;
    }
    let range = reference.range;
    let mut at =
        root.descendant_for_byte_range(range.start_byte as usize, range.end_byte as usize)?;
    while at.start_byte() != range.start_byte as usize || at.end_byte() != range.end_byte as usize {
        at = at.parent()?;
    }
    let call = std::iter::successors(Some(at), |n| n.parent())
        .take_while(|n| !matches!(n.kind(), "function_definition" | "lambda_expression"))
        .find(|n| n.kind() == "call_expression")?;
    let function = call.child_by_field_name("function")?;
    if function.start_byte() > range.start_byte as usize
        || function.end_byte() < range.end_byte as usize
    {
        return None;
    }
    let part = callee(function, source)?;
    let mut cursor = part.arguments.walk();
    let arguments = part
        .arguments
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .map(|n| super::declarations::template_type_argument(n, source))
        .collect();
    Some(CppTemplateCall {
        reference_id: reference.id,
        name: node_text(part.name, source)?,
        text: part.text,
        receiver: part.receiver,
        name_range: node_range(part.name),
        arguments_range: node_range(part.arguments),
        arguments,
    })
}

/// None on unsupported template syntax; Some(None) for ordinary declarations.
pub(super) fn function_parameters(name: Node<'_>, source: &str) -> Option<Option<Vec<String>>> {
    let declaration = std::iter::successors(name.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "function_definition" | "declaration" | "field_declaration"
        )
    })?;
    let templates: Vec<_> = std::iter::successors(declaration.parent(), |n| n.parent())
        .filter(|n| n.kind() == "template_declaration")
        .collect();
    if templates.is_empty() {
        return Some(None);
    }
    let [template] = templates.as_slice() else {
        return None;
    };
    if declaration.parent() != Some(*template) {
        // Preserve the already supported class-template operator signature path.
        let mut cursor = template.walk();
        return (name.kind() == "operator_name"
            && template.named_children(&mut cursor).any(|n| {
                matches!(n.kind(), "class_specifier" | "struct_specifier")
                    && n.start_byte() <= name.start_byte()
                    && name.end_byte() <= n.end_byte()
            }))
        .then_some(None);
    }
    let parameters = template.child_by_field_name("parameters")?;
    let mut cursor = template.walk();
    if template
        .named_children(&mut cursor)
        .any(|n| n != declaration && n != parameters && n.kind() != "comment")
    {
        return None;
    }
    let mut names = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
    {
        if parameter.kind() != "type_parameter_declaration" || parameter.has_error() {
            return None;
        }
        let id = single_child(parameter)?;
        if id.kind() != "type_identifier" {
            return None;
        }
        let value = node_text(id, source)?;
        if names.contains(&value) {
            return None;
        }
        names.push(value);
    }
    // Empty lists are explicit specializations, not primary templates.
    (!names.is_empty()).then_some(Some(names))
}
