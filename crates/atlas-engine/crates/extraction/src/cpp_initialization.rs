//! C++ declaration/initializer disambiguation for nested call expressions.
//!
//! A syntax-only grammar can read `Result r(f(x, y));` as a prototype. A
//! source-visible non-type name rules out a type-specifier at the relevant
//! position. Explicit expression parentheses then select the initializer parse;
//! unlike braces, they do not change constructor overload selection. The
//! inserted delimiters are projected out of tree coordinates before consumers
//! see the tree. Source bytes and the outer initialization form never change.

use std::collections::HashSet;
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

#[derive(Clone, Copy, PartialEq, Eq)]
enum NameKind {
    NonType,
    Type,
    Unknown,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}

fn declarator_name(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if matches!(node.kind(), "identifier" | "qualified_identifier") {
            return Some(node);
        }
        node = node.child_by_field_name("declarator")?;
    }
}

fn direct_name(mut node: Node<'_>) -> bool {
    while matches!(node.kind(), "init_declarator" | "function_declarator") {
        let Some(inner) = node.child_by_field_name("declarator") else {
            return false;
        };
        node = inner;
    }
    matches!(node.kind(), "identifier" | "qualified_identifier")
}

fn plain_type_name(node: Node<'_>) -> bool {
    match node.kind() {
        "type_identifier" | "namespace_identifier" => true,
        "qualified_identifier" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .all(|child| child.kind() == "comment" || plain_type_name(child))
        }
        _ => false,
    }
}

struct Names<'a> {
    source: &'a str,
    macros: HashSet<String>,
    canceled: &'a dyn Fn() -> bool,
}

impl Names<'_> {
    fn contains_macro(&self, node: Node<'_>) -> bool {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            if (self.canceled)() || self.macros.contains(text(node, self.source)) {
                return true;
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        false
    }

    fn lookup(&self, use_: Node<'_>, depth: usize) -> NameKind {
        if use_.kind() != "type_identifier" && use_.kind() != "identifier" {
            return NameKind::Unknown;
        }
        let name = text(use_, self.source);
        if depth > 16 || self.macros.contains(name) {
            return NameKind::Unknown;
        }
        for scope in std::iter::successors(use_.parent(), |n| n.parent()) {
            if (self.canceled)() {
                return NameKind::Unknown;
            }
            match scope.kind() {
                "compound_statement" | "declaration_list" | "translation_unit" => {
                    let mut cursor = scope.walk();
                    let children: Vec<_> = scope.named_children(&mut cursor).collect();
                    for child in children.into_iter().rev() {
                        if child.end_byte() > use_.start_byte() {
                            continue;
                        }
                        if let Some(kind) = self.declaration(child, name, depth) {
                            return kind;
                        }
                    }
                }
                "function_definition" => {
                    let Some(mut declarator) = scope.child_by_field_name("declarator") else {
                        return NameKind::Unknown;
                    };
                    while declarator.kind() != "function_declarator" {
                        let Some(inner) = declarator.child_by_field_name("declarator") else {
                            return NameKind::Unknown;
                        };
                        declarator = inner;
                    }
                    if declarator.has_error() || self.contains_macro(declarator) {
                        return NameKind::Unknown;
                    }
                    if let Some(parameters) = declarator.child_by_field_name("parameters") {
                        let mut cursor = parameters.walk();
                        for parameter in parameters.named_children(&mut cursor) {
                            if !parameter.has_error()
                                && parameter
                                    .child_by_field_name("declarator")
                                    .and_then(declarator_name)
                                    .is_some_and(|n| text(n, self.source) == name)
                            {
                                return NameKind::NonType;
                            }
                        }
                    }
                    // A qualified definition may be a member: an unseen member
                    // or base can hide a global function with the same name.
                    if declarator_name(declarator).is_none_or(|n| n.kind() != "identifier") {
                        return NameKind::Unknown;
                    }
                }
                "class_specifier"
                | "struct_specifier"
                | "lambda_expression"
                | "template_declaration"
                | "for_statement"
                | "for_range_loop"
                | "if_statement"
                | "switch_statement"
                | "catch_clause" => {
                    return NameKind::Unknown;
                }
                kind if kind.starts_with("preproc_") => return NameKind::Unknown,
                _ => {}
            }
        }
        NameKind::Unknown
    }

    fn declaration(&self, node: Node<'_>, name: &str, depth: usize) -> Option<NameKind> {
        if node.is_error() || node.kind().starts_with("preproc_") {
            return Some(NameKind::Unknown);
        }
        match node.kind() {
            "expression_statement" if self.contains_macro(node) => Some(NameKind::Unknown),
            "alias_declaration" | "class_specifier" | "struct_specifier" | "enum_specifier" => node
                .child_by_field_name("name")
                .filter(|n| text(*n, self.source) == name)
                .map(|_| {
                    if node.has_error() {
                        NameKind::Unknown
                    } else {
                        NameKind::Type
                    }
                }),
            // A using-directive can add names which this local lookup cannot
            // enumerate. Do not bypass it to choose an outer declaration.
            "using_declaration" => Some(NameKind::Unknown),
            "declaration" | "function_definition" | "type_definition" => {
                let mut cursor = node.walk();
                for declarator in node.children_by_field_name("declarator", &mut cursor) {
                    if declarator_name(declarator).is_none_or(|n| text(n, self.source) != name) {
                        continue;
                    }
                    if declarator.has_error() {
                        return Some(NameKind::Unknown);
                    }
                    if node
                        .child_by_field_name("type")
                        .is_some_and(|ty| self.contains_macro(ty))
                    {
                        return Some(NameKind::Unknown);
                    }
                    if node.kind() == "type_definition" {
                        return Some(NameKind::Type);
                    }
                    // A function and an object both introduce a non-type name.
                    // This does not claim that every declarator is a variable.
                    // But `T(x);` and `T * x;` can be expression statements, so
                    // those need a known type before x can be used as evidence.
                    let type_known = node.child_by_field_name("type").is_some_and(|ty| {
                        matches!(
                            ty.kind(),
                            "primitive_type"
                                | "sized_type_specifier"
                                | "placeholder_type_specifier"
                                | "struct_specifier"
                                | "class_specifier"
                        ) || self.lookup(ty, depth + 1) == NameKind::Type
                    });
                    // A template-looking head can instead be relational
                    // operators: `low < high > X();` does not declare X.
                    // Only plain type/name adjacency rules out that expression
                    // alternative without separately establishing the type.
                    let plain_head = node
                        .child_by_field_name("type")
                        .is_some_and(plain_type_name);
                    return Some(if direct_name(declarator) && plain_head || type_known {
                        NameKind::NonType
                    } else {
                        NameKind::Unknown
                    });
                }
                None
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct Expression {
    start: usize,
    end: usize,
    declaration_start: usize,
    declaration_end: usize,
}

fn initializer(node: Node<'_>, names: &Names<'_>) -> Option<Expression> {
    if node.kind() != "declaration"
        || node.has_error()
        || node.parent()?.kind() != "compound_statement"
    {
        return None;
    }
    let declarator = node.child_by_field_name("declarator")?;
    if declarator.kind() != "function_declarator" || !direct_name(declarator) {
        return None;
    }
    let parameters = declarator.child_by_field_name("parameters")?;
    let name = declarator.child_by_field_name("declarator")?;
    let mut cursor = declarator.walk();
    if declarator
        .named_children(&mut cursor)
        .any(|child| child != name && child != parameters && child.kind() != "comment")
    {
        // cv/ref/noexcept/trailing-return suffixes belong to a function
        // declarator; wrapping an argument cannot turn them into an initializer.
        return None;
    }
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter_declaration" {
            continue;
        }
        let Some(callee) = parameter.child_by_field_name("type") else {
            continue;
        };
        let Some(arguments) = parameter.child_by_field_name("declarator") else {
            continue;
        };
        // These alternatives spell a function-style expression. A bare name
        // is not wrapped: parentheses can affect decltype(auto) deduction.
        if !matches!(
            arguments.kind(),
            "abstract_function_declarator" | "parenthesized_declarator"
        ) {
            continue;
        }
        let callee_is_value = names.lookup(callee, 0) == NameKind::NonType;
        let argument_requires_value =
            arguments
                .child_by_field_name("parameters")
                .is_some_and(|args| {
                    let mut cursor = args.walk();
                    let args: Vec<_> = args
                        .named_children(&mut cursor)
                        .filter(|n| n.kind() != "comment")
                        .collect();
                    // A single `(x)` can declare a new parameter named x even when
                    // an outer x is a value. With multiple unnamed parameters, each
                    // bare name must instead be a type for the prototype alternative.
                    args.len() > 1
                        && args.iter().any(|arg| {
                            arg.kind() == "parameter_declaration"
                                && arg.child_by_field_name("declarator").is_none()
                                && arg
                                    .child_by_field_name("type")
                                    .is_some_and(|ty| names.lookup(ty, 0) == NameKind::NonType)
                        })
                });
        if callee_is_value || argument_requires_value {
            return Some(Expression {
                start: parameter.start_byte(),
                end: parameter.end_byte(),
                declaration_start: node.start_byte(),
                declaration_end: node.end_byte(),
            });
        }
    }
    None
}

fn advance(point: &mut Point, bytes: &[u8]) {
    for byte in bytes {
        if *byte == b'\n' {
            point.row += 1;
            point.column = 0;
        } else {
            point.column += 1;
        }
    }
}

pub(crate) fn refine(source: &str, original: Tree, canceled: &dyn Fn() -> bool) -> Option<Tree> {
    let mut pending = vec![original.root_node()];
    let mut declarations = Vec::new();
    let mut names = Names {
        source,
        macros: HashSet::new(),
        canceled,
    };
    while let Some(node) = pending.pop() {
        if canceled() {
            return None;
        }
        if matches!(node.kind(), "preproc_def" | "preproc_function_def") {
            if let Some(name) = node.child_by_field_name("name") {
                names.macros.insert(text(name, source).to_owned());
            }
        }
        if node.kind() == "declaration" {
            declarations.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    let expressions: Vec<_> = declarations
        .into_iter()
        .filter_map(|n| initializer(n, &names))
        .collect();
    if canceled() {
        return None;
    }
    if expressions.is_empty() {
        return Some(original);
    }
    let mut insertions: Vec<_> = expressions
        .iter()
        .flat_map(|e| [(e.start, b'('), (e.end, b')')])
        .collect();
    insertions.sort_unstable();
    let mut hinted = Vec::with_capacity(source.len() + insertions.len());
    let mut edits = Vec::new();
    let mut previous = 0;
    let mut point = Point::new(0, 0);
    for (offset, delimiter) in insertions {
        let slice = &source.as_bytes()[previous..offset];
        hinted.extend_from_slice(slice);
        advance(&mut point, slice);
        let start = hinted.len();
        edits.push(InputEdit {
            start_byte: start,
            old_end_byte: start + 1,
            new_end_byte: start,
            start_position: point,
            old_end_position: Point::new(point.row, point.column + 1),
            new_end_position: point,
        });
        hinted.push(delimiter);
        point.column += 1;
        previous = offset;
    }
    hinted.extend_from_slice(&source.as_bytes()[previous..]);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .ok()?;
    let mut progress = |_: &tree_sitter::ParseState| {
        if canceled() {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    tracing::debug!(target: "atlas_context_work", event = "refinement_parse", bytes = hinted.len());
    let mut tree = parser.parse_with_options(
        &mut |offset, _| hinted.get(offset..).unwrap_or(&[]),
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    )?;
    // Remove only synthetic delimiters from coordinates, retaining the chosen
    // expression structure. Reverse order keeps earlier edit positions stable.
    for edit in edits.iter().rev() {
        tree.edit(edit);
    }
    if tree.root_node().end_byte() != source.len() {
        return Some(original);
    }
    for expression in expressions {
        let declaration = tree
            .root_node()
            .descendant_for_byte_range(expression.declaration_start, expression.declaration_end)?;
        if declaration.has_error()
            || declaration.kind() != "declaration"
            || declaration
                .child_by_field_name("declarator")
                .is_none_or(|n| n.kind() != "init_declarator")
        {
            return Some(original);
        }
        let mut pending = vec![declaration];
        let mut call = false;
        while let Some(node) = pending.pop() {
            if canceled() {
                return None;
            }
            call |= node.kind() == "call_expression"
                && node.start_byte() == expression.start
                && node.end_byte() == expression.end;
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        if !call {
            return Some(original);
        }
    }
    Some(tree)
}
