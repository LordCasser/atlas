//! Retain the declaration subset needed by C++ receiver lookup.
use tree_sitter::Node;
use types::cpp::{
    CppBaseClass, CppDeclaredType, CppFileTypes, CppLookupLimit, CppMemberMacro, CppRecordType,
    CppValueType,
};
use types::{FileFacts, ReferenceKind, SymbolKind, TextRange};

use crate::cancel::CancelCheck;
use crate::languages::{node_range, node_text};

/// A standalone class declaration supplies identity without a member inventory.
/// An elaborated type in a value declaration (`struct Item* value`) needs its
/// own lookup; do not treat it as a standalone declaration in the value's scope.
fn forward_declaration(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if node.has_error()
        || node.child_by_field_name("body").is_some()
        || node
            .child_by_field_name("name")
            .is_none_or(|name| name.kind() != "type_identifier")
    {
        return false;
    }
    match parent.kind() {
        "translation_unit" | "declaration_list" => {
            std::iter::successors(node.next_sibling(), |next| next.next_sibling())
                .find(|next| next.kind() != "comment")
                .is_some_and(|next| next.kind() == ";" && !next.is_missing())
        }
        "declaration" | "field_declaration" => {
            let mut cursor = parent.walk();
            !parent.has_error()
                && parent
                    .child_by_field_name("type")
                    .is_some_and(|ty| ty.id() == node.id())
                && parent
                    .named_children(&mut cursor)
                    .all(|child| child.id() == node.id() || child.kind() == "comment")
        }
        _ => false,
    }
}

pub(crate) fn extract(
    root: Node<'_>,
    source: &str,
    facts: &FileFacts,
    token: &dyn CancelCheck,
) -> Option<CppFileTypes> {
    let mut result = CppFileTypes::default();
    let registry = crate::symbol_registry::SymbolRegistry::new(
        &facts.symbols,
        &facts.scopes,
        super::lambdas::caller_initializers(root, &facts.symbols),
    );
    let callable_ids: std::collections::HashSet<_> = facts
        .symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|symbol| symbol.id)
        .collect();
    // Until using/alias lookup is modeled, do not silently skip a declaration
    // that may hide a same-name class. This only gates type-based resolution.
    let mut cursor = root.walk();
    loop {
        if token.is_cancelled() {
            return None;
        }
        let node = cursor.node();
        if node.kind() == "new_expression" && crate::cpp_expressions::may_be_evaluated(node, source)
        {
            let range = node_range(node);
            result.allocation_sites.push(types::cpp::CppAllocationSite {
                range,
                source_symbol: registry
                    .source_for_range(range)
                    .filter(|id| callable_ids.contains(id)),
            });
        }
        if node.kind() == "lambda_expression" {
            let mut capture = lambda_capture(node, source);
            super::lambdas::annotate(node, source, facts, &registry, &mut capture);
            result.lambda_captures.push(capture);
        }
        crate::cpp_annotations::collect_node(node, source, &mut result);
        // A friend function is a namespace candidate exposed through its class.
        // Its identity is not modeled yet; retain the actual named limitation
        // without treating every ordinary member as a possible friend.
        if node.kind() == "function_declarator"
            && std::iter::successors(node.parent(), |n| n.parent())
                .take_while(|n| {
                    !matches!(
                        n.kind(),
                        "compound_statement" | "class_specifier" | "struct_specifier"
                    )
                })
                .any(|n| n.kind() == "friend_declaration")
        {
            let mut name = node.child_by_field_name("declarator");
            while name.is_some_and(|n| n.kind() == "qualified_identifier") {
                name = name.and_then(|n| n.child_by_field_name("name"));
            }
            result.adl_limits.push(CppLookupLimit {
                scope: super::qualified_name_from_node_cpp("", node, source)
                    .trim_end_matches("::")
                    .into(),
                name: name.and_then(|n| node_text(n, source)),
                declaration_range: node_range(node),
                block_range: None,
            });
        }
        if node.kind() == "namespace_definition" {
            let mut children = node.walk();
            if node.children(&mut children).any(|n| n.kind() == "inline") {
                let parent = super::qualified_name_from_node_cpp("", node, source)
                    .trim_end_matches("::")
                    .to_string();
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, source));
                let own = name.map(|name| {
                    if parent.is_empty() {
                        name
                    } else {
                        format!("{parent}::{name}")
                    }
                });
                for scope in std::iter::once(parent).chain(own) {
                    result.adl_limits.push(CppLookupLimit {
                        scope,
                        name: None,
                        declaration_range: node_range(node),
                        block_range: None,
                    });
                }
            }
        }
        // Keep all function-template names initially. Only a fully recorded
        // primary signature can discharge its own name-inventory limit below;
        // unsupported overloads/specializations must not disappear from lookup.
        if let Some(name) = super::templates::function_name(node, source) {
            result.lookup_limits.push(CppLookupLimit {
                scope: super::qualified_name_from_node_cpp("", node, source)
                    .trim_end_matches("::")
                    .to_string(),
                name: Some(name),
                declaration_range: node_range(node),
                block_range: None,
            });
        }
        if let Some((name, range)) = crate::cpp_member_macros::candidate(node, source) {
            let scope = super::qualified_name_from_node_cpp("", node, source)
                .trim_end_matches("::")
                .to_string();
            result.member_macros.push(CppMemberMacro {
                scope: scope.clone(),
                name,
                text: source
                    .get(range.start_byte as usize..range.end_byte as usize)?
                    .into(),
                range,
            });
            // Preserve the previous unknown inventory for whole declarations.
            // A prefix can also be an ordinary C++ declarator; it acquires a
            // macro limit only when an actual visible macro definition exists.
            if crate::cpp_member_macros::tokens(
                source.get(range.end_byte as usize..node.end_byte())?,
            )
            .is_some_and(|tokens| tokens.is_empty())
            {
                result.lookup_limits.push(CppLookupLimit {
                    scope,
                    name: None,
                    declaration_range: range,
                    block_range: None,
                });
            }
        }
        let template_owner = match node.kind() {
            "class_specifier" | "struct_specifier" => node.child_by_field_name("name"),
            "function_declarator" => node
                .child_by_field_name("declarator")
                .filter(|name| name.kind() == "qualified_identifier")
                .filter(|name| {
                    name.child_by_field_name("name").is_some_and(|name| {
                        name.kind() == "operator_name"
                            && node_text(name, source).is_some_and(|text| {
                                text.split_whitespace().collect::<String>() == "operator->"
                            })
                    })
                })
                .and_then(|name| name.child_by_field_name("scope")),
            _ => None,
        };
        if let Some((name, true)) =
            template_owner.and_then(|name| template_record_name(name, source))
        {
            let name = if name.starts_with("::") {
                name.trim_start_matches("::").into()
            } else {
                super::qualified_name_from_node_cpp(&name, node, source)
            };
            result.specialized_templates.push(name);
        }
        if matches!(
            cursor.node().kind(),
            "using_declaration"
                | "alias_declaration"
                | "type_definition"
                | "namespace_alias_definition"
        ) {
            // A named alias is already a TypeAlias symbol, so it only hides
            // that name. Do not disable every type lookup in its namespace.
            // Using imports can introduce names not represented by a symbol.
            let node = cursor.node();
            let named_alias = node.kind() != "using_declaration"
                && facts.symbols.iter().any(|s| {
                    s.kind == SymbolKind::TypeAlias
                        && s.name_range.start_byte as usize >= node.start_byte()
                        && s.name_range.end_byte as usize <= node.end_byte()
                });
            if !named_alias {
                result.lookup_limits.push(CppLookupLimit {
                    scope: super::qualified_name_from_node_cpp("", node, source)
                        .trim_end_matches("::")
                        .to_string(),
                    name: single_using_name(node, source),
                    declaration_range: node_range(node),
                    block_range: verified_block_range(root, node, facts),
                });
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return collect(root, source, facts, result, token);
            }
        }
    }
}

fn verified_block_range(root: Node<'_>, node: Node<'_>, facts: &FileFacts) -> Option<TextRange> {
    let block = std::iter::successors(node.parent(), |node| node.parent())
        .take_while(|node| {
            !matches!(
                node.kind(),
                "class_specifier" | "struct_specifier" | "namespace_definition"
            )
        })
        .find(|node| node.kind() == "compound_statement")?;
    let function = std::iter::successors(block.parent(), |node| node.parent())
        .find(|node| node.kind() == "function_definition")?;
    let declarator = function.child_by_field_name("declarator")?;
    facts
        .symbols
        .iter()
        .any(|symbol| {
            matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                && symbol.name_range.start_byte as usize >= declarator.start_byte()
                && symbol.name_range.end_byte as usize <= declarator.end_byte()
                && super::callables::scope_supported(root, symbol)
        })
        .then(|| node_range(block))
}

// A using-declaration introduces its terminal name. A using-directive, using
// enum, pack or malformed declaration can introduce an unknown set of names.
// Do not turn a recovered class's single named import into a namespace-wide ban.
fn single_using_name(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "using_declaration" || node.has_error() {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node
        .children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .collect();
    if children
        .iter()
        .any(|child| matches!(child.kind(), "namespace" | "enum" | "..." | ","))
    {
        return None;
    }
    let names: Vec<_> = children
        .iter()
        .copied()
        .filter(|child| child.is_named() && child.kind() != "type_qualifier")
        .collect();
    let [name] = names.as_slice() else {
        return None;
    };
    let mut name = *name;
    while name.kind() == "qualified_identifier" {
        name = name.child_by_field_name("name")?;
    }
    matches!(name.kind(), "identifier" | "type_identifier")
        .then(|| node_text(name, source))
        .flatten()
}

// Retain blockers even when the symbol query cannot extract specialized class
// names or out-of-class dependent member definitions. Otherwise an indexed
// primary template could silently replace a specialization's actual operator.
fn template_record_name(node: Node<'_>, source: &str) -> Option<(String, bool)> {
    match node.kind() {
        "type_identifier" | "identifier" | "namespace_identifier" => {
            Some((node_text(node, source)?, false))
        }
        "template_type" => Some((node_text(node.child_by_field_name("name")?, source)?, true)),
        "qualified_identifier" => {
            let (name, templated) =
                template_record_name(node.child_by_field_name("name")?, source)?;
            match node.child_by_field_name("scope") {
                Some(scope) => {
                    let (scope, parent_template) = template_record_name(scope, source)?;
                    Some((format!("{scope}::{name}"), templated || parent_template))
                }
                None => Some((format!("::{name}"), templated)),
            }
        }
        _ => None,
    }
}

fn collect(
    root: Node<'_>,
    source: &str,
    facts: &FileFacts,
    mut result: CppFileTypes,
    token: &dyn CancelCheck,
) -> Option<CppFileTypes> {
    for reference in &facts.references {
        if token.is_cancelled() {
            return None;
        }
        if reference.kind == ReferenceKind::Call
            && let Some(call) = super::templates::template_call(root, reference, source)
        {
            result.template_calls.push(call);
        }
    }
    for site in &facts.callsites {
        if token.is_cancelled() {
            return None;
        }
        if let Some(reference) = site.reference_id {
            result.arguments.push((
                reference,
                site.args
                    .iter()
                    .map(|arg| {
                        arg.range
                            .and_then(|range| at(root, range))
                            .map_or(types::cpp::CppArgumentExpression::Unknown, |node| {
                                argument_expression(node, source, 0)
                            })
                    })
                    .collect(),
            ));
        }
    }
    result
        .lookup_limits
        .sort_by_key(|limit| limit.declaration_range.start_byte);
    result.lookup_limits.dedup();
    result.specialized_templates.sort();
    result.specialized_templates.dedup();
    for binding in &facts.bindings {
        if token.is_cancelled() {
            return None;
        }
        let Some((declaration, ty)) = value_type(root, binding.range, source) else {
            continue;
        };
        let lookup_scope = binding
            .function_id
            .and_then(|id| facts.symbols.iter().find(|s| s.id == id))
            .map(|s| owner(&s.qualified_name))
            .unwrap_or_else(|| {
                super::qualified_name_from_node_cpp("", declaration, source)
                    .trim_end_matches("::")
                    .to_string()
            });
        result.values.push(CppValueType {
            binding_id: Some(binding.id),
            symbol_id: None,
            mutable_: false,
            bit_field: false,
            capture_required: {
                let mut cursor = declaration.walk();
                !declaration.children(&mut cursor).any(|child| {
                    matches!(
                        node_text(child, source).as_deref(),
                        Some("static" | "thread_local" | "extern")
                    )
                })
            },
            declared_type: ty,
            initializer_call: auto_initializer(root, binding.range, declaration, source, facts),
            lookup_scope,
            declaration_range: node_range(declaration),
        });
    }
    for symbol in &facts.symbols {
        if token.is_cancelled() {
            return None;
        }
        if symbol.kind == SymbolKind::TypeAlias {
            let declaration = at(root, symbol.name_range)
                .and_then(|n| ancestor(n, &["alias_declaration", "type_definition"]));
            let target = declaration.and_then(|node| alias_target(node, source));
            result.aliases.push(types::cpp::CppTypeAlias {
                symbol_id: symbol.id,
                target_range: target
                    .as_ref()
                    .map_or(symbol.name_range, |(_, range)| *range),
                target: target.map(|(ty, _)| ty),
            });
        }
        if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            let lambda = super::lambdas::node(root, symbol).is_some();
            let supported = if lambda {
                super::lambdas::scope_supported(root, symbol, facts)
            } else {
                super::callables::scope_supported(root, symbol)
            };
            if !supported {
                result.unverified_callable_scopes.push(symbol.id);
            } else if !lambda
                && let Some(callable) = super::callables::extract(root, symbol, source)
            {
                result.callables.push(callable);
            }
        }
        if matches!(symbol.kind, SymbolKind::Class | SymbolKind::Struct) {
            let Some(node) = at(root, symbol.name_range)
                .and_then(|n| ancestor(n, &["class_specifier", "struct_specifier"]))
            else {
                continue;
            };
            let mut cursor = node.walk();
            let base_clause = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "base_class_clause");
            let template_parameters = template_parameters(node, source);
            let identity_supported = (super::callables::record_scope_supported(node)
                || forward_declaration(node))
                && !has_ancestor(node, &["function_definition"])
                && (!has_ancestor(node, &["template_declaration"])
                    || template_parameters.is_some())
                && !std::iter::successors(node.parent(), |node| node.parent()).any(|node| {
                    node.kind() == "namespace_definition"
                        && node.child_by_field_name("name").is_none()
                });
            let lookup_supported = identity_supported
                && node.child_by_field_name("body").is_some()
                && !unmodeled_record_error(node, source);
            if node.child_by_field_name("body").is_some()
                && !super::callables::record_scope_supported(node)
            {
                // Recovered records cannot justify source ownership. Template
                // lookup limitations and forward declarations alone are not
                // malformed enclosing scopes (a forward may share a symbol ID
                // with the later valid definition).
                result.unverified_callable_scopes.push(symbol.id);
            }
            result.records.push(CppRecordType {
                symbol_id: symbol.id,
                is_definition: node.child_by_field_name("body").is_some(),
                bases: base_clause.map_or(Some(Vec::new()), |node| base_classes(node, source)),
                identity_supported,
                lookup_supported,
                template_parameters,
            });
        }
        if symbol.kind == SymbolKind::Field {
            let Some((declaration, ty)) = value_type(root, symbol.name_range, source) else {
                continue;
            };
            result.values.push(CppValueType {
                binding_id: None,
                symbol_id: Some(symbol.id),
                capture_required: false,
                mutable_: {
                    let mut cursor = declaration.walk();
                    declaration
                        .children(&mut cursor)
                        .any(|child| node_text(child, source).as_deref() == Some("mutable"))
                },
                // A declaration may mix bit-fields and ordinary declarators.
                bit_field: at(root, symbol.name_range)
                    .and_then(|name| name.next_named_sibling())
                    .is_some_and(|next| next.kind() == "bitfield_clause"),
                declared_type: ty,
                initializer_call: None,
                lookup_scope: owner(&symbol.qualified_name),
                declaration_range: node_range(declaration),
            });
        }
    }
    for reference in &facts.references {
        if token.is_cancelled() {
            return None;
        }
        if reference.kind == ReferenceKind::Call
            && at(root, reference.range).is_some_and(|node| {
                std::iter::successors(Some(node), |node| node.parent())
                    .take_while(|node| {
                        !matches!(
                            node.kind(),
                            "function_definition" | "class_specifier" | "struct_specifier"
                        )
                    })
                    .filter(|node| node.kind() == "lambda_expression")
                    .any(|lambda| !captures_this(lambda))
            })
        {
            result.this_capture_unavailable.push(reference.id);
        }
    }
    result.lookup_limits.retain(|limit| {
        !at(root, limit.declaration_range).is_some_and(|node| {
            node.kind() == "template_declaration"
                && result.callables.iter().any(|callable| {
                    callable.template_parameters.is_some()
                        && facts.symbols.iter().any(|symbol| {
                            symbol.id == callable.symbol_id
                                && limit.declaration_range.start_byte
                                    <= symbol.name_range.start_byte
                                && symbol.name_range.end_byte <= limit.declaration_range.end_byte
                        })
                })
        })
    });
    Some(result)
}

fn alias_target(node: Node<'_>, source: &str) -> Option<(CppDeclaredType, TextRange)> {
    // Resolve only ordinary namespace/class aliases with a plain target type.
    // Pointer/reference/cv composition, dependent and local alias binding need
    // facts not represented by this identity-only subset.
    if node.has_error()
        || has_ancestor(
            node,
            &[
                "template_declaration",
                "function_definition",
                "lambda_expression",
            ],
        )
        || std::iter::successors(node.parent(), |n| n.parent()).any(|n| {
            (matches!(n.kind(), "class_specifier" | "struct_specifier")
                && !super::callables::record_scope_supported(n))
                || (n.kind() == "namespace_definition" && n.child_by_field_name("name").is_none())
                || n.is_error()
        })
    {
        return None;
    }
    let target = node.child_by_field_name("type")?;
    let type_node = if target.kind() == "type_descriptor" {
        let ty = target.child_by_field_name("type")?;
        let mut cursor = target.walk();
        if target
            .named_children(&mut cursor)
            .any(|n| n.id() != ty.id() && n.kind() != "comment")
        {
            return None;
        }
        ty
    } else {
        target
    };
    let mut cursor = node.walk();
    if node.named_children(&mut cursor).any(|child| {
        child.id() != target.id()
            && child.kind() != "comment"
            && !(matches!(
                child.kind(),
                "type_identifier" | "identifier" | "primitive_type"
            ) && super::cpp_alias_name(child, source).is_some())
    }) {
        return None;
    }
    let ty = declared_type(type_node, source)?;
    if !ty.template_arguments.is_empty() || ty.pointer || ty.reference || ty.const_ || ty.volatile {
        return None;
    }
    Some((ty, node_range(type_node)))
}

fn unmodeled_record_error(node: Node<'_>, source: &str) -> bool {
    if !node.has_error() && !node.is_missing() {
        return false;
    }
    if node.kind() == "function_definition" {
        let Some(body) = node.child_by_field_name("body") else {
            return true;
        };
        // A delimited member-function body is a separate declaration scope.
        // Its statements do not add class members. Inspect the full signature
        // and require real body delimiters, while retaining body extraction
        // failures separately instead of invalidating the class name inventory.
        if body.kind() != "compound_statement"
            || !body
                .child(0)
                .is_some_and(|n| n.kind() == "{" && !n.is_missing())
            || !body
                .children(&mut body.walk())
                .last()
                .is_some_and(|n| n.kind() == "}" && !n.is_missing())
        {
            return true;
        }
        let mut cursor = node.walk();
        return node
            .children(&mut cursor)
            .any(|child| child.id() != body.id() && unmodeled_record_error(child, source));
    }
    if unnamed_pointer_template_parameter(node, source) {
        return false;
    }
    // The current grammar can recover `argument = {}` as a compound literal
    // with a missing type. An empty braced default is valid C++ and cannot add
    // or rename a class member. Do not reject every unrelated member for it.
    // The affected callable retains its unsupported signature independently.
    if node.is_missing()
        && node.kind() == "type_identifier"
        && let Some(literal) = node
            .parent()
            .filter(|n| n.kind() == "compound_literal_expression")
        && node_text(literal, source)
            .is_some_and(|text| text.split_whitespace().collect::<String>() == "{}")
        && let Some(parameter) = literal
            .parent()
            .filter(|n| n.kind() == "optional_parameter_declaration")
        && parameter
            .child_by_field_name("default_value")
            .is_some_and(|n| n.id() == literal.id())
    {
        return false;
    }
    if node.is_error() || node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| unmodeled_record_error(child, source))
}

fn member_function_template_name(template: Node<'_>) -> Option<Node<'_>> {
    if template.kind() != "template_declaration"
        || template
            .parent()
            .is_none_or(|p| p.kind() != "field_declaration_list")
    {
        return None;
    }
    let parameters = template.child_by_field_name("parameters")?;
    let mut cursor = template.walk();
    let mut declarations = template
        .named_children(&mut cursor)
        .filter(|n| n.id() != parameters.id() && n.kind() != "comment");
    let declaration = declarations.next()?;
    if declarations.next().is_some()
        || !matches!(
            declaration.kind(),
            "function_definition" | "field_declaration" | "declaration"
        )
        || declaration.has_error()
    {
        return None;
    }
    let declarator = declaration.child_by_field_name("declarator")?;
    if declarator.kind() != "function_declarator" {
        return None;
    }
    declarator
        .child_by_field_name("declarator")
        .filter(|name| matches!(name.kind(), "identifier" | "field_identifier"))
}

// The grammar recovers the abstract declarator in `Type* = nullptr` as an
// ERROR containing only `*`. In a member function's template parameter list
// this cannot introduce another class member or change its name. Preserve
// the function-template symbol for hiding; callables::extract still declines
// its dependent signature. Other errors, including in its body, remain limits.
fn unnamed_pointer_template_parameter(error: Node<'_>, source: &str) -> bool {
    if !error.is_error() || node_text(error, source).as_deref() != Some("*") {
        return false;
    }
    let Some(parameter) = error
        .parent()
        .filter(|n| n.kind() == "optional_parameter_declaration")
    else {
        return false;
    };
    let Some(parameters) = parameter
        .parent()
        .filter(|n| n.kind() == "template_parameter_list")
    else {
        return false;
    };
    let Some(template) = parameters.parent().filter(|n| {
        n.kind() == "template_declaration"
            && n.parent()
                .is_some_and(|p| p.kind() == "field_declaration_list")
    }) else {
        return false;
    };
    let (Some(ty), Some(default)) = (
        parameter.child_by_field_name("type"),
        parameter.child_by_field_name("default_value"),
    ) else {
        return false;
    };
    if parameter.child_by_field_name("declarator").is_some()
        || ty.has_error()
        || default.has_error()
    {
        return false;
    }
    let mut cursor = parameter.walk();
    if !parameter.children(&mut cursor).all(|n| {
        n.id() == ty.id()
            || n.id() == default.id()
            || n.id() == error.id()
            || matches!(n.kind(), "=" | "comment")
    }) {
        return false;
    }
    member_function_template_name(template).is_some()
}

fn template_parameters(node: Node<'_>, source: &str) -> Option<Vec<String>> {
    let template = node
        .parent()
        .filter(|n| n.kind() == "template_declaration")?;
    if has_ancestor(
        template,
        &[
            "template_declaration",
            "class_specifier",
            "struct_specifier",
        ],
    ) || node.child_by_field_name("name")?.kind() != "type_identifier"
    {
        return None;
    }
    let parameters = template.child_by_field_name("parameters")?;
    let mut cursor = template.walk();
    if template.named_children(&mut cursor).any(|child| {
        child.id() != node.id() && child.id() != parameters.id() && child.kind() != "comment"
    }) {
        return None;
    }
    let mut names = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() == "comment" {
            continue;
        }
        if parameter.kind() != "type_parameter_declaration" || parameter.has_error() {
            return None;
        }
        let mut cursor = parameter.walk();
        let identifiers: Vec<_> = parameter
            .named_children(&mut cursor)
            .filter(|child| child.kind() != "comment")
            .collect();
        let [name] = identifiers.as_slice() else {
            return None;
        };
        if name.kind() != "type_identifier" {
            return None;
        }
        let name = node_text(*name, source)?;
        if names.contains(&name) {
            return None;
        }
        names.push(name);
    }
    (!names.is_empty()).then_some(names)
}

fn lambda_capture(lambda: Node<'_>, source: &str) -> types::cpp::CppLambdaCapture {
    use types::cpp::{CppCaptureKind as Kind, CppLambdaCapture};
    let body = lambda.child_by_field_name("body");
    let declarator = lambda.child_by_field_name("declarator");
    let mut result = CppLambdaCapture {
        symbol_id: None,
        enclosing_symbol: None,
        binding_id: None,
        binding_const: false,
        parameter_count: None,
        range: node_range(lambda),
        body_range: node_range(body.unwrap_or(lambda)),
        default: None,
        captures: None,
        mutable_: declarator.is_some_and(|node| {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .any(|child| node_text(child, source).as_deref() == Some("mutable"))
        }),
    };
    let Some(captures) = lambda.child_by_field_name("captures") else {
        return result;
    };
    if captures.has_error()
        || captures.is_missing()
        || body.is_none()
        || declarator.is_some_and(|node| node.has_error())
    {
        return result;
    }
    let mut cursor = captures.walk();
    let children: Vec<_> = captures
        .children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "[" | "]" | "comment"))
        .collect();
    let mut names = Vec::new();
    for group in children.split(|node| node.kind() == ",") {
        if group.is_empty() {
            if children.is_empty() {
                break;
            }
            return result;
        }
        let (node, kind) = match group {
            [node] if node.kind() == "lambda_default_capture" => {
                result.default = match node_text(*node, source).as_deref() {
                    Some("=") => Some(Kind::Copy),
                    Some("&") => Some(Kind::Reference),
                    _ => return result,
                };
                continue;
            }
            [node] if node.kind() == "this" => continue,
            [star, node] if star.kind() == "*" && node.kind() == "this" => continue,
            [node] => (*node, Kind::Copy),
            [amp, node] if amp.kind() == "&" => (*node, Kind::Reference),
            _ => return result,
        };
        let (name, kind) = match node.kind() {
            "identifier" => (node_text(node, source), kind),
            "lambda_capture_initializer" => (
                node.child_by_field_name("left")
                    .and_then(|name| node_text(name, source)),
                Kind::Unknown,
            ),
            // Packs require instantiation; do not borrow an outer same-name type.
            "parameter_pack_expansion" => {
                let mut cursor = node.walk();
                (
                    node.named_children(&mut cursor)
                        .find(|child| child.kind() == "identifier")
                        .and_then(|name| node_text(name, source)),
                    Kind::Unknown,
                )
            }
            _ => return result,
        };
        let Some(name) = name else {
            return result;
        };
        if names.iter().any(|(old, _)| old == &name) {
            return result;
        }
        names.push((name, kind));
    }
    result.captures = Some(names);
    result
}

fn captures_this(lambda: Node<'_>) -> bool {
    let Some(captures) = lambda
        .child_by_field_name("captures")
        .filter(|node| !node.has_error())
    else {
        return false;
    };
    let mut cursor = captures.walk();
    let mut has_this = false;
    for child in captures.children(&mut cursor) {
        // Copy capture (*this) and implicit/default capture need separate
        // object/capture semantics. An explicit pointer capture keeps `this`.
        if child.kind() == "*" {
            return false;
        }
        has_this |= child.kind() == "this";
    }
    has_this
}

fn base_classes(node: Node<'_>, source: &str) -> Option<Vec<CppBaseClass>> {
    let mut bases = Vec::new();
    let mut virtual_ = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            ":" | "access_specifier" | "comment" => {}
            "virtual" => virtual_ = true,
            "," => virtual_ = false,
            "type_identifier" | "qualified_identifier" | "template_type" => {
                bases.push(CppBaseClass {
                    declared_type: declared_type(child, source)?,
                    virtual_,
                    range: node_range(child),
                })
            }
            _ => return None,
        }
    }
    Some(bases)
}

fn owner(name: &str) -> String {
    name.rsplit_once("::")
        .map_or("", |(owner, _)| owner)
        .to_string()
}

fn at(root: Node<'_>, range: TextRange) -> Option<Node<'_>> {
    root.descendant_for_byte_range(range.start_byte as usize, range.end_byte as usize)
}

fn ancestor<'a>(mut node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    loop {
        if kinds.contains(&node.kind()) {
            return Some(node);
        }
        if matches!(node.kind(), "compound_statement" | "translation_unit") {
            return None;
        }
        node = node.parent()?;
    }
}

fn has_ancestor(mut node: Node<'_>, kinds: &[&str]) -> bool {
    while let Some(parent) = node.parent() {
        if kinds.contains(&parent.kind()) {
            return true;
        }
        node = parent;
    }
    false
}

fn value_type<'a>(
    root: Node<'a>,
    location: TextRange,
    source: &str,
) -> Option<(Node<'a>, Option<CppDeclaredType>)> {
    let name = at(root, location)?;
    let declaration = ancestor(
        name,
        &[
            "declaration",
            "field_declaration",
            "parameter_declaration",
            "optional_parameter_declaration",
        ],
    )?;
    let parse = || {
        if declaration.has_error() || has_ancestor(declaration, &["template_declaration"]) {
            return None;
        }
        let type_node = declaration.child_by_field_name("type")?;
        let mut ty = declared_type(type_node, source)?;
        let mut cursor = declaration.walk();
        for child in declaration.named_children(&mut cursor) {
            if child.kind() == "type_qualifier" {
                match node_text(child, source)?.as_str() {
                    "const" => ty.const_ = true,
                    "volatile" => ty.volatile = true,
                    "mutable" if declaration.kind() == "field_declaration" => {}
                    _ => return None,
                }
            }
        }
        let mut node = name;
        while node.id() != declaration.id() {
            match node.kind() {
                "identifier" | "field_identifier" | "init_declarator" => {}
                "pointer_declarator" if !ty.pointer && !ty.reference => ty.pointer = true,
                "reference_declarator" if !ty.pointer && !ty.reference => ty.reference = true,
                _ => return None,
            }
            node = node.parent()?;
        }
        Some(ty)
    };
    Some((declaration, parse()))
}

fn plain_type_name(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "namespace_identifier" => node_text(node, source),
        "qualified_identifier" => {
            let name = plain_type_name(node.child_by_field_name("name")?, source)?;
            match node.child_by_field_name("scope") {
                Some(scope) => Some(format!("{}::{name}", plain_type_name(scope, source)?)),
                None => Some(format!("::{name}")),
            }
        }
        // auto, decltype, template instantiations, arrays and function pointers
        // need their own semantics; retaining None still preserves shadowing.
        _ => None,
    }
}

fn auto_initializer(
    root: Node<'_>,
    location: TextRange,
    declaration: Node<'_>,
    source: &str,
    facts: &FileFacts,
) -> Option<types::ReferenceId> {
    if declaration.has_error() || has_ancestor(declaration, &["template_declaration"]) {
        return None;
    }
    if node_text(declaration.child_by_field_name("type")?, source)?.as_str() != "auto" {
        return None;
    }
    let mut cursor = declaration.walk();
    if declaration
        .named_children(&mut cursor)
        .any(|node| node.kind() == "type_qualifier")
    {
        return None;
    }
    // Start with plain auto = call(...). Reference deduction, braced
    // initialization, conditional expressions and decltype(auto) are separate.
    let name = at(root, location)?;
    let initializer = name.parent()?;
    if initializer.kind() != "init_declarator" {
        return None;
    }
    let value = initializer.child_by_field_name("value")?;
    if value.kind() != "call_expression" {
        return None;
    }
    let callee = value.child_by_field_name("function")?;
    let mut calls = facts.references.iter().filter(|r| {
        r.kind == ReferenceKind::Call
            && r.range.start_byte as usize >= callee.start_byte()
            && r.range.end_byte as usize <= callee.end_byte()
    });
    let call = calls.next()?;
    calls.next().is_none().then_some(call.id)
}

pub(super) fn return_type(
    declaration: Node<'_>,
    function: Node<'_>,
    source: &str,
) -> Option<CppDeclaredType> {
    let mut ty = declared_type(declaration.child_by_field_name("type")?, source)?;
    let mut cursor = declaration.walk();
    for node in declaration.named_children(&mut cursor) {
        if node.kind() == "type_qualifier" {
            match node_text(node, source)?.as_str() {
                "const" => ty.const_ = true,
                "volatile" => ty.volatile = true,
                _ => return None,
            }
        }
    }
    let mut node = function.parent()?;
    while node.id() != declaration.id() {
        match node.kind() {
            "pointer_declarator" if !ty.pointer && !ty.reference => ty.pointer = true,
            "reference_declarator" if !ty.pointer && !ty.reference => ty.reference = true,
            _ => return None,
        }
        node = node.parent()?;
    }
    Some(ty)
}

fn argument_expression(
    node: Node<'_>,
    source: &str,
    depth: usize,
) -> types::cpp::CppArgumentExpression {
    use types::cpp::CppArgumentExpression as Expr;
    if depth >= 16 || node.has_error() {
        return Expr::Unknown;
    }
    match node.kind() {
        "identifier" => node_text(node, source).map_or(Expr::Unknown, Expr::Name),
        "field_expression" => match (
            node.child_by_field_name("argument"),
            node.child_by_field_name("field"),
            node.child_by_field_name("operator")
                .and_then(|n| node_text(n, source)),
        ) {
            (Some(object), Some(field), Some(operator))
                if field.kind() == "field_identifier"
                    && matches!(operator.as_str(), "." | "->") =>
            {
                Expr::Field {
                    object: Box::new(argument_expression(object, source, depth + 1)),
                    name: node_text(field, source).unwrap_or_default(),
                    arrow: operator == "->",
                }
            }
            _ => Expr::Unknown,
        },
        "number_literal" | "char_literal" | "true" | "false" => {
            node_text(node, source).map_or(Expr::Unknown, Expr::Literal)
        }
        "parenthesized_expression" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node
                .named_children(&mut cursor)
                .filter(|n| n.kind() != "comment")
                .collect();
            match children.as_slice() {
                [child] => argument_expression(*child, source, depth + 1),
                _ => Expr::Unknown,
            }
        }
        "binary_expression" => match (
            node.child_by_field_name("operator")
                .and_then(|n| node_text(n, source)),
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            (Some(operator), Some(left), Some(right)) => Expr::Binary {
                operator,
                left: Box::new(argument_expression(left, source, depth + 1)),
                right: Box::new(argument_expression(right, source, depth + 1)),
            },
            _ => Expr::Unknown,
        },
        _ => Expr::Unknown,
    }
}

pub(super) fn parameter_type(node: Node<'_>, source: &str) -> Option<CppDeclaredType> {
    let mut ty = declared_type(node.child_by_field_name("type")?, source)?;
    let mut cursor = node.walk();
    for child in node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "type_qualifier")
    {
        match node_text(child, source)?.as_str() {
            "const" => ty.const_ = true,
            "volatile" => ty.volatile = true,
            _ => return None,
        }
    }
    let mut declarator = node.child_by_field_name("declarator");
    while let Some(node) = declarator {
        match node.kind() {
            "identifier" | "field_identifier" => break,
            "pointer_declarator" | "abstract_pointer_declarator"
                if !ty.pointer && !ty.reference =>
            {
                ty.pointer = true
            }
            "reference_declarator" | "abstract_reference_declarator"
                if !ty.reference && !ty.pointer =>
            {
                ty.reference = true
            }
            _ => return None,
        }
        declarator = node.child_by_field_name("declarator").or_else(|| {
            let mut children = node.walk();
            node.named_children(&mut children)
                .find(|n| n.kind() != "type_qualifier" && n.kind() != "comment")
        });
    }
    if !ty.pointer && !ty.reference {
        ty.const_ = false;
        ty.volatile = false;
    }
    Some(ty)
}

pub(super) fn template_type_argument(node: Node<'_>, source: &str) -> Option<CppDeclaredType> {
    if node.kind() != "type_descriptor"
        || node.has_error()
        || node.child_by_field_name("declarator").is_some()
    {
        return None;
    }
    let type_node = node.child_by_field_name("type")?;
    let mut ty = declared_type(type_node, source)?;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child == type_node || child.kind() == "comment" {
            continue;
        }
        if child.kind() != "type_qualifier" {
            return None;
        }
        match node_text(child, source)?.as_str() {
            "const" => ty.const_ = true,
            "volatile" => ty.volatile = true,
            _ => return None,
        }
    }
    Some(ty)
}

fn declared_type(node: Node<'_>, source: &str) -> Option<CppDeclaredType> {
    let mut ty = CppDeclaredType {
        name: String::new(),
        template_arguments: Vec::new(),
        pointer: false,
        reference: false,
        const_: false,
        volatile: false,
    };
    match node.kind() {
        "primitive_type" | "sized_type_specifier" => {
            ty.name = node_text(node, source)?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
        }
        "template_type" => {
            ty.name = plain_type_name(node.child_by_field_name("name")?, source)?;
            let args = node.child_by_field_name("arguments")?;
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                if arg.kind() == "comment" {
                    continue;
                }
                if arg.kind() != "type_descriptor"
                    || arg.child_by_field_name("declarator").is_some()
                {
                    return None;
                }
                let type_node = arg.child_by_field_name("type")?;
                let mut item = declared_type(type_node, source)?;
                let mut cursor = arg.walk();
                for child in arg.named_children(&mut cursor) {
                    if child.id() == type_node.id() || child.kind() == "comment" {
                        continue;
                    }
                    match node_text(child, source)?.as_str() {
                        "const" => item.const_ = true,
                        "volatile" => item.volatile = true,
                        _ => return None,
                    }
                }
                ty.template_arguments.push(item);
            }
            // Empty template arguments may require defaults/substitution.
            if ty.template_arguments.is_empty() {
                return None;
            }
        }
        "qualified_identifier" => {
            ty = declared_type(node.child_by_field_name("name")?, source)?;
            let prefix = match node.child_by_field_name("scope") {
                Some(scope) => Some(plain_type_name(scope, source)?),
                None => None,
            };
            ty.name = format!("{}::{}", prefix.unwrap_or_default(), ty.name);
        }
        _ => ty.name = plain_type_name(node, source)?,
    }
    Some(ty)
}
