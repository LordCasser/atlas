//! Byte-stable declaration annotation normalization from actual macro definitions.
//!
//! This recognizes empty/visibility/import-export and limited analysis-only
//! thread attributes whose every supplied definition has that shape. It is not a preprocessor;
//! callers must supply the definitions visible at the use in their input scope.
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

#[cfg(feature = "cpp")]
use tree_sitter::Node;
use types::{
    Language,
    cpp::{CppAnnotationPosition, CppAnnotationToken, CppMacroDefinition, CppMemberMacro},
};
#[cfg(feature = "cpp")]
use types::{TextRange, cpp::CppFileTypes};

use crate::{LanguageFrontend, ParserSpec, create_frontend};

/// Use the same byte-preserving parser input during indexing and investigation.
/// The original source remains available to all capture normalizers/readers.
pub fn frontend(
    annotations: Vec<CppAnnotationToken>,
    members: Vec<CppMemberMacro>,
) -> Option<LanguageFrontend> {
    let mut frontend = create_frontend(Language::Cpp)?;
    frontend.parser = Box::new(AnnotationParser {
        inner: frontend.parser,
        annotations,
        members,
    });
    Some(frontend)
}

struct AnnotationParser {
    inner: Box<dyn ParserSpec>,
    annotations: Vec<CppAnnotationToken>,
    members: Vec<CppMemberMacro>,
}
impl ParserSpec for AnnotationParser {
    fn language(&self) -> Language {
        Language::Cpp
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        self.inner.tree_sitter_language()
    }
    fn refine_tree(
        &self,
        source: &str,
        tree: tree_sitter::Tree,
        canceled: &dyn Fn() -> bool,
    ) -> Option<tree_sitter::Tree> {
        self.inner.refine_tree(source, tree, canceled)
    }
    fn parser_source<'a>(&self, source: &'a str) -> Cow<'a, str> {
        if self.annotations.is_empty() && self.members.is_empty() {
            return Cow::Borrowed(source);
        }
        let mut normalized = source.as_bytes().to_vec();
        for (range, text) in self
            .annotations
            .iter()
            .map(|site| (site.range, &site.text))
            .chain(self.members.iter().map(|site| (site.range, &site.text)))
        {
            let Some(token) =
                normalized.get_mut(range.start_byte as usize..range.end_byte as usize)
            else {
                continue;
            };
            // Match the actual recorded token, preserving unrelated source if
            // the caller's file changed since extraction.
            if token == text.as_bytes() && !token.is_empty() {
                for byte in token {
                    if !matches!(*byte, b'\n' | b'\r') {
                        *byte = b' ';
                    }
                }
            }
        }
        Cow::Owned(String::from_utf8(normalized).expect("whole matched ranges were blanked"))
    }
}

#[cfg(feature = "cpp")]
pub(crate) fn collect_node(node: Node<'_>, source: &str, facts: &mut CppFileTypes) {
    use crate::languages::{node_range, node_text};
    if matches!(
        node.kind(),
        "preproc_def" | "preproc_function_def" | "preproc_call"
    ) && inactive_literal_branch(node, source)
    {
        return;
    }
    match node.kind() {
        "preproc_def" | "preproc_function_def" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(n, source))
            {
                let parameter_list = node
                    .child_by_field_name("parameters")
                    .and_then(|n| node_text(n, source))
                    .and_then(|text| crate::cpp_member_macros::parameters(&text));
                let (parameters, variadic) = parameter_list
                    .map(|(names, variadic)| (Some(names), variadic))
                    .unwrap_or((None, false));
                let replacement = (!node.has_error()
                    && (node.kind() == "preproc_def" || parameters.is_some()))
                .then(|| {
                    node.child_by_field_name("value")
                        .and_then(|n| node_text(n, source))
                        .unwrap_or_default()
                });
                facts.macros.push(CppMacroDefinition {
                    name,
                    parameters,
                    variadic,
                    replacement,
                    range: node_range(node),
                });
            }
        }
        "preproc_call" => {
            if node
                .child_by_field_name("directive")
                .and_then(|n| node_text(n, source))
                .as_deref()
                == Some("#undef")
                && let Some(argument) = node
                    .child_by_field_name("argument")
                    .and_then(|n| node_text(n, source))
                && let Some(tokens) = tokens(&argument)
                && let [name] = tokens.as_slice()
                && identifier(name)
            {
                facts.macros.push(CppMacroDefinition {
                    name: (*name).into(),
                    parameters: None,
                    variadic: false,
                    replacement: None,
                    range: node_range(node),
                });
            }
        }
        "class_specifier" | "struct_specifier" => {
            let mut cursor = node.walk();
            let Some(keyword) = node
                .children(&mut cursor)
                .find(|n| matches!(n.kind(), "class" | "struct"))
            else {
                return;
            };
            // Attributes between the class-key and name belong to the class.
            // Keep them intact and locate the following macro, instead of
            // treating their opening brackets as the end of the class head.
            let mut anchor = keyword;
            let mut cursor = node.walk();
            for child in node
                .children(&mut cursor)
                .filter(|n| n.start_byte() >= keyword.end_byte())
            {
                if !matches!(
                    child.kind(),
                    "attribute_declaration" | "attribute_specifier" | "comment"
                ) || child.has_error()
                    || !source[anchor.end_byte()..child.start_byte()]
                        .trim()
                        .is_empty()
                {
                    break;
                }
                anchor = child;
            }
            let Some((start, end)) = following_identifier(source, anchor.end_byte()) else {
                return;
            };
            // A second identifier is required: do not erase a lone class name.
            if following_identifier(source, end).is_none() {
                return;
            }
            let prefix = &source[anchor.end_byte()..start];
            let newlines = prefix.bytes().filter(|b| *b == b'\n').count();
            let line = anchor.end_position().row + newlines;
            let column = if newlines == 0 {
                anchor.end_position().column + prefix.len()
            } else {
                prefix.rsplit('\n').next().unwrap_or("").len()
            };
            facts.annotation_candidates.push(CppAnnotationToken {
                name: source[start..end].into(),
                text: source[start..end].into(),
                position: CppAnnotationPosition::ClassPrefix,
                range: TextRange {
                    start_byte: start as u32,
                    end_byte: end as u32,
                    start_line: line as u32,
                    end_line: line as u32,
                    start_column: column as u32,
                    end_column: (column + end - start) as u32,
                },
            });
        }
        "field_declaration" | "declaration" | "function_definition" => {
            // A leading annotation can be recovered as the return/data type,
            // even without an ERROR (e.g. an annotated constructor). Require
            // its own plain type token and a following identifier. Actual
            // macro definitions are checked before any byte is normalized;
            // named types and semantic specifiers are never erased by spelling.
            if let Some(ty) = node.child_by_field_name("type").filter(|ty| {
                ty.kind() == "type_identifier"
                    && {
                        let mut cursor = node.walk();
                        node.children(&mut cursor)
                            .take_while(|child| child.id() != ty.id())
                            .all(|child| {
                                matches!(
                                    child.kind(),
                                    "attribute_declaration" | "attribute_specifier" | "comment"
                                ) && !child.has_error()
                            })
                    }
                    && !ty.has_error()
                    && following_identifier(source, ty.end_byte()).is_some()
            }) && let Some(text) = node_text(ty, source)
            {
                facts.annotation_candidates.push(CppAnnotationToken {
                    name: text.clone(),
                    text,
                    range: node_range(ty),
                    position: CppAnnotationPosition::DeclarationPrefix,
                });
            }
        }
        "function_declarator" => {
            collect_suffixes(node, source, facts, CppAnnotationPosition::CallableSuffix);
        }
        "parameter_list" if lambda_parameters(node, source) => {
            collect_suffixes(node, source, facts, CppAnnotationPosition::LambdaSuffix);
        }
        "init_declarator" if recovered_empty_function_declarator(node, source) => {
            collect_suffixes(node, source, facts, CppAnnotationPosition::CallableSuffix);
        }
        "identifier" | "field_identifier" | "type_identifier" | "namespace_identifier" => {
            if data_declarator_context(node) {
                collect_suffixes(node, source, facts, CppAnnotationPosition::DataSuffix);
            }
        }
        _ => {}
    }
}

#[cfg(feature = "cpp")]
fn recovered_empty_function_declarator(node: Node<'_>, source: &str) -> bool {
    // `auto name() ANNOTATION(x) { ... }` can make the annotation the
    // recovered function's name and put `name()` in an ERROR initializer.
    // Only accept the complete, empty-parameter spelling after an actual type;
    // arbitrary ERROR text, value initializers and macro replacements stay unknown.
    let Some(error) = node
        .parent()
        .filter(|parent| parent.is_error() && parent.byte_range() == node.byte_range())
    else {
        return false;
    };
    let Some(function) = error
        .parent()
        .filter(|parent| parent.kind() == "function_definition")
    else {
        return false;
    };
    let Some(ty) = function
        .child_by_field_name("type")
        .filter(|ty| !ty.has_error())
    else {
        return false;
    };
    ty.end_byte() <= node.start_byte()
        && source[ty.end_byte()..node.start_byte()].trim().is_empty()
        && node.child_by_field_name("declarator").is_some_and(|name| {
            !name.has_error() && matches!(name.kind(), "identifier" | "qualified_identifier")
        })
        && node.child_by_field_name("value").is_some_and(|value| {
            value.kind() == "argument_list"
                && crate::cpp_member_macros::tokens(&source[value.byte_range()])
                    == Some(vec!["(".into(), ")".into()])
        })
        && function
            .child_by_field_name("declarator")
            .is_some_and(|declarator| {
                node.end_byte() < declarator.start_byte()
                    && source[node.end_byte()..declarator.start_byte()]
                        .trim()
                        .is_empty()
            })
}

#[cfg(feature = "cpp")]
fn inactive_literal_branch(mut node: Node<'_>, source: &str) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "preproc_if" | "preproc_elif")
            && let Some(condition) = parent.child_by_field_name("condition")
            && let Some(condition) = tokens(&source[condition.byte_range()])
        {
            let alternative = parent
                .child_by_field_name("alternative")
                .is_some_and(|alternative| alternative.id() == node.id());
            if (condition == ["0"] && !alternative) || (condition == ["1"] && alternative) {
                return true;
            }
        }
        node = parent;
    }
    false
}

#[cfg(feature = "cpp")]
fn lambda_parameters(node: Node<'_>, source: &str) -> bool {
    let previous = node.prev_named_sibling().or_else(|| {
        node.parent()
            .filter(|parent| parent.kind() == "abstract_function_declarator")
            .and_then(|parent| parent.prev_named_sibling())
    });
    previous.is_some_and(|capture| {
        capture.kind() == "lambda_capture_specifier"
            && source[capture.end_byte()..node.start_byte()]
                .trim()
                .is_empty()
    })
}

#[cfg(feature = "cpp")]
fn data_declarator_context(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        match parent.kind() {
            "parameter_list"
            | "argument_list"
            | "compound_statement"
            | "call_expression"
            | "preproc_arg"
            | "preproc_def"
            | "preproc_function_def" => return false,
            "init_declarator" => {
                if parent.child_by_field_name("value").is_some_and(|value| {
                    value.start_byte() <= node.start_byte() && node.end_byte() <= value.end_byte()
                }) {
                    return false;
                }
            }
            "declaration" | "field_declaration" => {
                return parent
                    .child_by_field_name("type")
                    .is_some_and(|ty| ty.end_byte() <= node.start_byte());
            }
            "function_definition" | "class_specifier" | "struct_specifier" => return false,
            _ => {}
        }
        ancestor = parent.parent();
    }
    false
}

#[cfg(feature = "cpp")]
fn collect_suffixes(
    anchor: Node<'_>,
    source: &str,
    facts: &mut CppFileTypes,
    position: CppAnnotationPosition,
) {
    let mut after = anchor.end_byte();
    let mut sites = Vec::new();
    for _ in 0..32 {
        let Some((start, name_end)) = following_identifier(source, after) else {
            return;
        };
        let mut end = name_end;
        let whitespace = source[end..].len() - source[end..].trim_start().len();
        if source.as_bytes().get(end + whitespace) == Some(&b'(') {
            let Some(close) = invocation_end(source, end + whitespace) else {
                return;
            };
            end = close;
        } else if position == CppAnnotationPosition::DataSuffix {
            // Data declarator recovery is less specific than a callable. Only
            // explicit function-like spellings are considered in this position.
            return;
        }
        let prefix = &source[anchor.end_byte()..start];
        let lines = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let column = if lines == 0 {
            anchor.end_position().column + prefix.len()
        } else {
            prefix.rsplit('\n').next().unwrap_or("").len()
        };
        let text = &source[start..end];
        let inner_lines = text.bytes().filter(|byte| *byte == b'\n').count();
        sites.push(CppAnnotationToken {
            name: source[start..name_end].into(),
            text: text.into(),
            position: position.clone(),
            range: TextRange {
                start_byte: start as u32,
                end_byte: end as u32,
                start_line: (anchor.end_position().row + lines) as u32,
                end_line: (anchor.end_position().row + lines + inner_lines) as u32,
                start_column: column as u32,
                end_column: if inner_lines == 0 {
                    (column + text.len()) as u32
                } else {
                    text.rsplit('\n').next().unwrap_or("").len() as u32
                },
            },
        });
        let rest = source[end..].trim_start();
        if rest.starts_with([';', '{'])
            || (position == CppAnnotationPosition::DataSuffix && rest.starts_with('='))
            || (matches!(
                position,
                CppAnnotationPosition::CallableSuffix | CppAnnotationPosition::LambdaSuffix
            ) && rest.starts_with("->"))
        {
            facts.annotation_candidates.extend(sites);
            return;
        }
        after = end;
    }
}

/// Find the end of a parenthesized invocation without interpreting its contents.
/// Quoted strings/comments cannot prematurely close the argument list.
pub(crate) fn invocation_end(source: &str, start: usize) -> Option<usize> {
    let bytes = &source.as_bytes()[..start.saturating_add(64 * 1024).min(source.len())];
    let mut at = start;
    let mut depth = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            b'"' | b'\'' => {
                let quote = bytes[at];
                at += 1;
                while *bytes.get(at)? != quote {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while !(bytes.get(at) == Some(&b'*') && bytes.get(at + 1) == Some(&b'/')) {
                    bytes.get(at)?;
                    at += 1;
                }
                at += 1;
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while bytes.get(at).is_some_and(|byte| *byte != b'\n') {
                    at += 1;
                }
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

fn following_identifier(source: &str, after: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut start = after;
    while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    if !bytes
        .get(start)
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        return None;
    }
    let mut end = start + 1;
    while bytes
        .get(end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        end += 1;
    }
    Some((start, end))
}

fn identifier(s: &str) -> bool {
    following_identifier(s, 0) == Some((0, s.len()))
}

/// Accept only supported annotation replacements in all visible alternatives.
/// Expansion is bounded to complete annotation wrappers, sequences and aliases.
/// A selector may return a function-like name followed by its invocation. Arguments
/// requiring macro prescanning, token pasting and stringification stay unknown.
pub fn is_annotation(
    text: &str,
    definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
    position: &CppAnnotationPosition,
) -> bool {
    fn visit(
        input: Vec<String>,
        definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
        path: &mut HashSet<String>,
        remaining: &mut usize,
        position: &CppAnnotationPosition,
        depth: usize,
    ) -> Option<()> {
        if depth >= 32 {
            return None;
        }
        let bytes = input.iter().map(String::len).sum::<usize>();
        *remaining = remaining.checked_sub(bytes)?;
        if input.is_empty() {
            return Some(());
        }
        let end = invocation_tokens_end(&input).unwrap_or(1);
        let borrowed: Vec<_> = input[..end].iter().map(String::as_str).collect();
        if attribute(&borrowed, position)
            && !input[..end]
                .iter()
                .any(|token| definitions.contains_key(token.as_str()))
        {
            return visit(
                input[end..].to_vec(),
                definitions,
                path,
                remaining,
                position,
                depth + 1,
            );
        }
        // A shadowed attribute wrapper still follows ordinary macro expansion.
        // Inspect every actual replacement below; its spelling alone cannot
        // establish either a harmless annotation or a semantic declaration.
        let name = input.first()?;
        if !identifier(name) || path.len() >= 32 || !path.insert(name.clone()) {
            return None;
        }
        let answer = (|| {
            let alternatives = definitions.get(name.as_str())?;
            if alternatives.is_empty() {
                return None;
            }
            for definition in alternatives {
                let replacement =
                    crate::cpp_member_macros::tokens(definition.replacement.as_ref()?)?;
                let (expanded, tail) = if let Some(parameters) = &definition.parameters {
                    let mut arguments = invocation_arguments(&input[1..end])?;
                    if arguments == [Vec::<String>::new()] && parameters.is_empty() {
                        arguments.clear();
                    }
                    if arguments.len() < parameters.len()
                        || (!definition.variadic && arguments.len() != parameters.len())
                        || arguments.iter().any(|argument| {
                            argument.iter().enumerate().any(|(index, token)| {
                                definitions.get(token.as_str()).is_some_and(|alternatives| {
                                    // A function-like macro name without a following `(`
                                    // is not expanded during argument prescanning. This
                                    // permits selectors without evaluating macro arguments.
                                    alternatives
                                        .iter()
                                        .any(|definition| definition.parameters.is_none())
                                        || argument.get(index + 1).is_some_and(|next| next == "(")
                                })
                            })
                        })
                    {
                        return None;
                    }
                    let variadic: Vec<String> = arguments[parameters.len()..]
                        .iter()
                        .enumerate()
                        .flat_map(|(i, argument)| {
                            let mut items = Vec::new();
                            if i > 0 {
                                items.push(",".into());
                            }
                            items.extend(argument.clone());
                            items
                        })
                        .collect();
                    let mut expanded = Vec::new();
                    let mut expanded_bytes = 0usize;
                    for token in replacement {
                        let additions = if let Some(index) =
                            parameters.iter().position(|parameter| *parameter == token)
                        {
                            arguments[index].as_slice()
                        } else if definition.variadic && token == "__VA_ARGS__" {
                            variadic.as_slice()
                        } else {
                            std::slice::from_ref(&token)
                        };
                        expanded_bytes += additions.iter().map(String::len).sum::<usize>();
                        if expanded_bytes > *remaining || expanded.len() + additions.len() > 8192 {
                            return None;
                        }
                        expanded.extend(additions.iter().cloned());
                    }
                    (expanded, &input[end..])
                } else {
                    (replacement, &input[1..])
                };
                if tail.first().is_some_and(|token| token == "(") {
                    // Only a complete alias may consume the following call.
                    // Do not compose arbitrary replacement fragments with source.
                    let [alias] = expanded.as_slice() else {
                        return None;
                    };
                    if !identifier(alias) || expanded.len() + tail.len() > 8192 {
                        return None;
                    }
                    let mut invocation = expanded;
                    invocation.extend_from_slice(tail);
                    visit(
                        invocation,
                        definitions,
                        path,
                        remaining,
                        position,
                        depth + 1,
                    )?;
                } else {
                    visit(expanded, definitions, path, remaining, position, depth + 1)?;
                    // The following independent annotation can use the same macro;
                    // recursion through its replacement remains rejected above.
                    let mut following_path = path.clone();
                    following_path.remove(name);
                    visit(
                        tail.to_vec(),
                        definitions,
                        &mut following_path,
                        remaining,
                        position,
                        depth + 1,
                    )?;
                }
            }
            Some(())
        })();
        path.remove(name);
        answer
    }
    let Some(input) = crate::cpp_member_macros::tokens(text) else {
        return false;
    };
    visit(
        input,
        definitions,
        &mut HashSet::new(),
        &mut (64 * 1024),
        position,
        0,
    )
    .is_some()
}

// End of the first whole name(argument-list), not the last `)` in a sequence.
fn invocation_tokens_end(tokens: &[String]) -> Option<usize> {
    if tokens.get(1)?.as_str() != "(" {
        return None;
    }
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(1) {
        match token.as_str() {
            "(" => depth += 1,
            ")" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn invocation_arguments(tokens: &[String]) -> Option<Vec<Vec<String>>> {
    if tokens.first()?.as_str() != "(" || tokens.last()?.as_str() != ")" {
        return None;
    }
    let mut depth = 0usize;
    let mut arguments = vec![Vec::new()];
    for token in &tokens[1..tokens.len() - 1] {
        match token.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.checked_sub(1)?,
            "," if depth == 0 => {
                arguments.push(Vec::new());
                continue;
            }
            _ => {}
        }
        arguments.last_mut()?.push(token.clone());
    }
    (depth == 0).then_some(arguments)
}

fn attribute(tokens: &[&str], position: &CppAnnotationPosition) -> bool {
    match tokens {
        [
            "__attribute__" | "__attribute",
            "(",
            "(",
            "visibility" | "__visibility__",
            "(",
            value,
            ")",
            ")",
            ")",
        ] => {
            *position != CppAnnotationPosition::LambdaSuffix
                && matches!(
                    *value,
                    "\"default\"" | "\"hidden\"" | "\"protected\"" | "\"internal\""
                )
        }
        [
            "__attribute__" | "__attribute",
            "(",
            "(",
            "dllexport" | "dllimport" | "__dllexport__" | "__dllimport__",
            ")",
            ")",
        ] => *position != CppAnnotationPosition::LambdaSuffix,
        ["__declspec", "(", "dllexport" | "dllimport", ")"] => {
            matches!(
                position,
                CppAnnotationPosition::ClassPrefix | CppAnnotationPosition::DeclarationPrefix
            )
        }
        ["__attribute__" | "__attribute", "(", "(", name, rest @ ..]
            if rest.ends_with(&[")", ")"]) =>
        {
            let arguments = &rest[..rest.len() - 2];
            match (position, *name) {
                (
                    CppAnnotationPosition::CallableSuffix | CppAnnotationPosition::LambdaSuffix,
                    "no_thread_safety_analysis",
                ) => arguments.is_empty(),
                (
                    CppAnnotationPosition::CallableSuffix | CppAnnotationPosition::LambdaSuffix,
                    "requires_capability"
                    | "requires_shared_capability"
                    | "exclusive_locks_required"
                    | "shared_locks_required"
                    | "locks_excluded"
                    | "acquire_capability"
                    | "release_capability",
                )
                | (CppAnnotationPosition::DataSuffix, "guarded_by" | "pt_guarded_by") => {
                    simple_capability_arguments(arguments)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

// Bounded capability designators, not arbitrary constant expressions or macro
// argument evaluation. Keep side effects, braces and injected declarations out.
fn simple_capability_arguments(tokens: &[&str]) -> bool {
    let Some(inner) = tokens
        .strip_prefix(&["("])
        .and_then(|t| t.strip_suffix(&[")"]))
    else {
        return false;
    };
    !inner.is_empty()
        && inner.split(|token| *token == ",").all(|mut argument| {
            if let [literal] = argument
                && literal.starts_with('"')
                && literal.ends_with('"')
            {
                return true;
            }
            if matches!(argument.first(), Some(&"&" | &"*")) {
                argument = &argument[1..];
            }
            if argument.first() == Some(&"::") {
                argument = &argument[1..];
            }
            let Some((first, rest)) = argument.split_first() else {
                return false;
            };
            identifier(first)
                && rest.len() % 2 == 0
                && rest
                    .chunks_exact(2)
                    .all(|part| matches!(part[0], "::" | "->" | ".") && identifier(part[1]))
        })
}

// Small preprocessing-token reader: no evaluation, substitution or concatenation.
// Preserve string contents so malformed visibility values cannot become valid
// merely by removing whitespace. Comments and line splices are separators.
fn tokens(source: &str) -> Option<Vec<&str>> {
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut result = Vec::new();
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if source[at..].starts_with("\\\n") {
            at += 2;
            continue;
        }
        if source[at..].starts_with("//") {
            break;
        }
        if source[at..].starts_with("/*") {
            at += 2 + source[at + 2..].find("*/")? + 2;
            continue;
        }
        let start = at;
        if bytes[at].is_ascii_alphabetic() || bytes[at] == b'_' {
            at += 1;
            while bytes
                .get(at)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                at += 1;
            }
        } else if bytes[at] == b'"' {
            at += 1;
            while *bytes.get(at)? != b'"' {
                if bytes[at] == b'\\' {
                    at += 1;
                }
                at += 1;
            }
            at += 1;
        } else if bytes[at].is_ascii() {
            at += 1;
        } else {
            return None;
        }
        result.push(&source[start..at]);
    }
    Some(result)
}
