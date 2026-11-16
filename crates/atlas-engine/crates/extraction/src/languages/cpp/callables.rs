//! Written parameter identities for C++ callable declarations.
//! Primary function-template parameters remain separate from argument substitution.
use tree_sitter::Node;
use types::{SymbolDef, cpp::CppCallableDeclaration};

use crate::languages::node_text;

pub(super) fn extract(
    root: Node<'_>,
    symbol: &SymbolDef,
    source: &str,
) -> Option<CppCallableDeclaration> {
    let name = root.descendant_for_byte_range(
        symbol.name_range.start_byte as usize,
        symbol.name_range.end_byte as usize,
    )?;
    let template_parameters = super::templates::function_parameters(name, source)?;
    let anonymous_namespace =
        std::iter::successors(name.parent(), |node| node.parent()).any(|node| {
            node.kind() == "namespace_definition" && node.child_by_field_name("name").is_none()
        });
    let function = std::iter::successors(name.parent(), |node| node.parent())
        .take_while(|node| !matches!(node.kind(), "compound_statement" | "translation_unit"))
        .find(|node| node.kind() == "function_declarator")?;
    let parameters = function.child_by_field_name("parameters")?;
    if function.has_error() {
        return None;
    }
    let mut cursor = parameters.walk();
    if parameters
        .children(&mut cursor)
        .any(|node| node.kind() == "...")
    {
        return None;
    }
    let mut result = CppCallableDeclaration {
        symbol_id: symbol.id,
        template_parameters,
        parameter_types: Vec::new(),
        parameter_declared_types: Vec::new(),
        minimum_arity: 0,
        qualifiers: String::new(),
        is_virtual: false,
        internal_linkage: false,
        return_type: None,
    };
    let mut saw_default = false;
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() == "comment" {
            continue;
        }
        if !matches!(
            parameter.kind(),
            "parameter_declaration" | "optional_parameter_declaration"
        ) {
            return None;
        }
        let default = parameter.child_by_field_name("default_value").is_some();
        let mut cursor = parameter.walk();
        if parameter
            .children(&mut cursor)
            .any(|node| node.kind() == "this")
        {
            return None;
        }
        if !default {
            if saw_default {
                return None;
            }
            result.minimum_arity += 1;
        }
        saw_default |= default;
        result
            .parameter_types
            .push(written_type(parameter, source, true)?);
        result
            .parameter_declared_types
            .push(super::declarations::parameter_type(parameter, source));
    }
    if result.parameter_types == ["void"] {
        result.parameter_types.clear();
        result.parameter_declared_types.clear();
        result.minimum_arity = 0;
    }
    let declarator = function.child_by_field_name("declarator")?;
    let mut qualifiers = Vec::new();
    let mut cursor = function.walk();
    for child in function.named_children(&mut cursor) {
        if child.id() == declarator.id() || child.id() == parameters.id() {
            continue;
        }
        match child.kind() {
            "type_qualifier" | "ref_qualifier" => qualifiers.push(node_text(child, source)?),
            "virtual_specifier"
            | "noexcept"
            | "throw_specifier"
            | "trailing_return_type"
            | "comment" => {}
            _ => return None,
        }
    }
    qualifiers.sort();
    result.qualifiers = qualifiers.join(" ");
    let declaration =
        std::iter::successors(function.parent(), |node| node.parent()).find(|node| {
            matches!(
                node.kind(),
                "field_declaration" | "declaration" | "function_definition"
            )
        })?;
    let mut cursor = declaration.walk();
    result.is_virtual = declaration
        .children(&mut cursor)
        .any(|child| child.kind() == "virtual");
    let mut cursor = declaration.walk();
    result.internal_linkage = anonymous_namespace
        || (declaration.named_children(&mut cursor).any(|child| {
            child.kind() == "storage_class_specifier"
                && node_text(child, source).as_deref() == Some("static")
        }) && !std::iter::successors(declaration.parent(), |node| node.parent())
            .any(|node| matches!(node.kind(), "class_specifier" | "struct_specifier")));
    result.return_type = super::declarations::return_type(declaration, function, source);
    Some(result)
}

pub(super) fn scope_supported(root: Node<'_>, symbol: &SymbolDef) -> bool {
    let Some(name) = root.descendant_for_byte_range(
        symbol.name_range.start_byte as usize,
        symbol.name_range.end_byte as usize,
    ) else {
        return false;
    };
    let Some(declaration) = std::iter::successors(name.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "function_definition" | "declaration" | "field_declaration"
        )
    }) else {
        return false;
    };
    // A recovery AST may parse `class EXPORT Name { ... }` as a malformed
    // outer function containing clean-looking function definitions. C++ named
    // functions cannot nest directly. Keep their source symbols for inspection
    // but do not use flattened owners for either caller or callee lookup.
    let mut parent = declaration.parent();
    while let Some(node) = parent {
        match node.kind() {
            "class_specifier" | "struct_specifier" => {
                return record_scope_supported(node);
            }
            "namespace_definition" | "translation_unit" => {
                return namespace_callable_scope_supported(declaration, name);
            }
            "function_definition" | "lambda_expression" | "ERROR" | "friend_declaration" => {
                return false;
            }
            _ => parent = node.parent(),
        }
    }
    true
}

fn namespace_callable_scope_supported(declaration: Node<'_>, name: Node<'_>) -> bool {
    let declarator = std::iter::successors(Some(name), |node| node.parent())
        .take_while(|node| node.id() != declaration.id())
        .find(|node| node.kind() == "function_declarator");
    if let Some(node) = declarator {
        if node
            .child_by_field_name("declarator")
            .is_some_and(|name| name.kind() == "qualified_identifier")
        {
            // An out-of-class definition spells its owner explicitly.
            return true;
        }
        let mut cursor = node.walk();
        let member_qualifier = node.named_children(&mut cursor).any(|child| {
            matches!(
                child.kind(),
                "type_qualifier" | "ref_qualifier" | "virtual_specifier"
            )
        });
        let mut cursor = declaration.walk();
        return !member_qualifier
            && !declaration
                .children(&mut cursor)
                .any(|child| child.kind() == "virtual");
    }
    false
}

pub(super) fn record_scope_supported(node: Node<'_>) -> bool {
    let body = node.child_by_field_name("body");
    let mut cursor = node.walk();
    body.is_some()
        && node
            .children(&mut cursor)
            .all(|child| body.is_some_and(|body| child.id() == body.id()) || !child.has_error())
}

/// Canonicalize only the supported AST syntax, omitting the declarator name.
/// Array/function parameter adjustment and dependent expressions are deferred.
fn written_type(node: Node<'_>, source: &str, parameter: bool) -> Option<String> {
    let type_node = node.child_by_field_name("type")?;
    let mut result = type_name(type_node, source)?;
    let mut qualifiers = Vec::new();
    let declarator = node.child_by_field_name("declarator");
    let default = node.child_by_field_name("default_value");
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.id() == type_node.id()
            || declarator.is_some_and(|node| node.id() == child.id())
            || default.is_some_and(|node| node.id() == child.id())
        {
            continue;
        }
        match child.kind() {
            "type_qualifier" => qualifiers.push(cv(child, source)?),
            "comment" => {}
            _ => return None,
        }
    }
    qualifiers.sort();
    let mut layers = Vec::new();
    if let Some(declarator) = declarator {
        declarator_layers(declarator, source, &mut layers)?;
    }
    // Top-level cv does not distinguish function parameter types. Pointee and
    // referenced-type qualifiers do, and must survive this adjustment.
    if parameter {
        match layers.last_mut() {
            Some((operator, cv)) if operator == "*" => cv.clear(),
            None => qualifiers.clear(),
            _ => {}
        }
    }
    if !qualifiers.is_empty() {
        result = format!("{} {result}", qualifiers.join(" "));
    }
    for (operator, cv) in layers {
        result.push_str(&operator);
        if !cv.is_empty() {
            result.push(' ');
            result.push_str(&cv.join(" "));
        }
    }
    Some(result)
}

fn cv(node: Node<'_>, source: &str) -> Option<String> {
    let text = node_text(node, source)?;
    matches!(text.as_str(), "const" | "volatile").then_some(text)
}

fn declarator_layers(
    node: Node<'_>,
    source: &str,
    layers: &mut Vec<(String, Vec<String>)>,
) -> Option<()> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(()),
        "pointer_declarator" | "abstract_pointer_declarator" => {
            let child_declarator = node.child_by_field_name("declarator");
            let mut qualifiers = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child_declarator.is_some_and(|node| node.id() == child.id()) {
                    continue;
                }
                match child.kind() {
                    "type_qualifier" => qualifiers.push(cv(child, source)?),
                    "comment" => {}
                    _ => return None,
                }
            }
            qualifiers.sort();
            layers.push(("*".into(), qualifiers));
            if let Some(child) = child_declarator {
                declarator_layers(child, source, layers)?;
            }
            Some(())
        }
        "reference_declarator" | "abstract_reference_declarator" => {
            let mut cursor = node.walk();
            let operator = node
                .children(&mut cursor)
                .find(|child| matches!(child.kind(), "&" | "&&"))?;
            layers.push((operator.kind().into(), Vec::new()));
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "comment" {
                    declarator_layers(child, source, layers)?;
                }
            }
            Some(())
        }
        _ => None,
    }
}

fn type_name(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "namespace_identifier" | "primitive_type" => {
            node_text(node, source)
        }
        "sized_type_specifier" => {
            // Keep fundamental spelling conservative (e.g. unsigned vs unsigned
            // int may still fail to associate), never collapse distinct tokens.
            let mut cursor = node.walk();
            let mut tokens = Vec::new();
            for child in node.children(&mut cursor) {
                if child.kind() != "comment" {
                    tokens.push(node_text(child, source)?);
                }
            }
            Some(tokens.join(" "))
        }
        "qualified_identifier" => {
            let name = type_name(node.child_by_field_name("name")?, source)?;
            match node.child_by_field_name("scope") {
                Some(scope) => Some(format!("{}::{name}", type_name(scope, source)?)),
                None => Some(format!("::{name}")),
            }
        }
        "template_type" | "template_function" => {
            let name = type_name(node.child_by_field_name("name")?, source)?;
            let arguments = node.child_by_field_name("arguments")?;
            let mut values = Vec::new();
            let mut cursor = arguments.walk();
            for argument in arguments.named_children(&mut cursor) {
                match argument.kind() {
                    "type_descriptor" => values.push(written_type(argument, source, false)?),
                    // C++ parsing cannot always classify a nested template
                    // argument as type vs value without name lookup. Retain
                    // its supported written form for declaration association;
                    // this does not instantiate it or infer its value/type.
                    "qualified_identifier" | "identifier" | "template_function" => {
                        values.push(type_name(argument, source)?);
                    }
                    "number_literal" | "true" | "false" => {
                        values.push(node_text(argument, source)?)
                    }
                    "comment" => {}
                    _ => return None,
                }
            }
            Some(format!("{name}<{}>", values.join(",")))
        }
        _ => None,
    }
}
