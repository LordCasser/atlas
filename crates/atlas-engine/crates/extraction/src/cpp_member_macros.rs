//! Bounded name inventories for actual class-member macro definitions.
//!
//! This does not publish expanded symbols or callable bodies. It can narrow an
//! unknown-name lookup restriction to the names introduced by every supplied
//! replacement. Unsupported preprocessing or declaration syntax stays unknown.
use std::collections::{HashMap, HashSet};

#[cfg(feature = "cpp")]
use tree_sitter::{Node, Parser};
use types::cpp::{CppMacroDefinition, CppMemberMacro};

const MAX_FRAGMENT_BYTES: usize = 64 * 1024;

// Only the token subset needed for declaration fragments. Splices precede
// tokenization; quoted literals are indivisible, including encoding prefixes.
pub(crate) fn tokens(source: &str) -> Option<Vec<String>> {
    if source.len() > MAX_FRAGMENT_BYTES {
        return None;
    }
    let source = source.replace("\\\r\n", "").replace("\\\n", "");
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut result = Vec::new();
    while at < bytes.len() {
        let remaining = &source[at..];
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if remaining.starts_with("//") {
            at += remaining.find('\n').unwrap_or(remaining.len());
            continue;
        }
        if let Some(comment) = remaining.strip_prefix("/*") {
            at += 2 + comment.find("*/")? + 2;
            continue;
        }
        let start = at;
        // [lex.pptoken]: <:: followed by neither ':' nor '>' starts a '<'
        // token, not the '<:' digraph. Qualified template arguments therefore
        // have the same tokens with or without whitespace after '<'.
        let qualified_after_less =
            remaining.starts_with("<::") && !matches!(bytes.get(at + 3), Some(b':' | b'>'));
        if ["<:", ":>", "<%", "%>", "%:"]
            .iter()
            .any(|token| remaining.starts_with(token) && !(*token == "<:" && qualified_after_less))
        {
            return None;
        }
        let quote = [
            "u8\"", "u\"", "U\"", "L\"", "u8'", "u'", "U'", "L'", "\"", "'",
        ]
        .into_iter()
        .find(|prefix| remaining.starts_with(prefix));
        if let Some(prefix) = quote {
            at += prefix.len();
            let delimiter = bytes[at - 1];
            while *bytes.get(at)? != delimiter {
                if bytes[at] == b'\n' || bytes[at] == b'\r' {
                    return None;
                }
                if bytes[at] == b'\\' {
                    at += 1;
                }
                at += 1;
            }
            at += 1;
            // User-defined literal suffixes need their own token semantics.
            if bytes
                .get(at)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                return None;
            }
        } else if bytes[at].is_ascii_alphabetic() || bytes[at] == b'_' {
            at += 1;
            while bytes
                .get(at)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                at += 1;
            }
            // Raw string literals are outside this subset.
            if bytes.get(at) == Some(&b'"') {
                return None;
            }
        } else if bytes[at].is_ascii_digit() {
            at += 1;
            while bytes
                .get(at)
                .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'.'))
            {
                at += 1;
            }
        } else if let Some(operator) = [
            "->*", "...", "##", "::", "->", "&&", "||", "<<", ">>", "==", "!=", "<=", ">=", "++",
            "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", ".*",
        ]
        .into_iter()
        .find(|operator| remaining.starts_with(operator))
        {
            at += operator.len();
        } else if b"(){}[];,:*=&<>+-/!~?.|^#".contains(&bytes[at]) {
            at += 1;
        } else {
            // Includes digraphs, non-ASCII identifiers and raw splices.
            return None;
        }
        result.push(source[start..at].to_string());
        if result.len() > 8192 {
            return None;
        }
    }
    Some(result)
}

fn identifier(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(feature = "cpp")]
pub(crate) fn parameters(source: &str) -> Option<(Vec<String>, bool)> {
    let tokens = tokens(source)?;
    if tokens.first()?.as_str() != "(" || tokens.last()?.as_str() != ")" {
        return None;
    }
    let inner = &tokens[1..tokens.len() - 1];
    if inner.is_empty() {
        return Some((Vec::new(), false));
    }
    let mut names = Vec::new();
    let mut parts = inner.split(|token| token == ",").peekable();
    while let Some(parameter) = parts.next() {
        let [name] = parameter else { return None };
        if name == "..." {
            return parts.peek().is_none().then_some((names, true));
        }
        if !identifier(name) || name == "__VA_ARGS__" || names.contains(name) {
            return None;
        }
        names.push(name.clone());
    }
    Some((names, false))
}

fn invocation(source: &str) -> Option<(String, Vec<Vec<String>>, bool)> {
    let tokens = tokens(source)?;
    let name = tokens.first()?;
    if !identifier(name) || tokens.get(1)?.as_str() != "(" {
        return None;
    }
    let mut arguments = vec![Vec::new()];
    let mut depth = 0usize;
    for (offset, token) in tokens.iter().enumerate().skip(2) {
        match token.as_str() {
            ")" if depth == 0 => {
                let tail = &tokens[offset + 1..];
                return (tail.is_empty() || tail == [";"])
                    .then(|| (name.clone(), arguments, !tail.is_empty()));
            }
            "(" => depth += 1,
            ")" => depth -= 1,
            "," if depth == 0 => {
                arguments.push(Vec::new());
                continue;
            }
            _ => {}
        }
        arguments.last_mut()?.push(token.clone());
    }
    None
}

// A wrapper may consist entirely of complete member-macro invocations without
// separators. Check each replacement independently: a following invocation must
// not supply a missing terminator or complete another macro's declaration.
#[cfg(feature = "cpp")]
fn invocation_sequence(
    fragment: &str,
    definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
) -> Option<Vec<(String, String)>> {
    let tokens = tokens(fragment)?;
    let mut at = 0;
    let mut sites = Vec::new();
    while at < tokens.len() {
        if tokens[at] == ";" {
            at += 1;
            continue;
        }
        let start = at;
        let name = &tokens[at];
        if !identifier(name)
            || !definitions.contains_key(name.as_str())
            || tokens.get(at + 1)?.as_str() != "("
        {
            return None;
        }
        at += 2;
        let mut depth = 1usize;
        while depth != 0 {
            match tokens.get(at)?.as_str() {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            at += 1;
        }
        if tokens.get(at).is_some_and(|token| token == ";") {
            at += 1;
        }
        sites.push((name.clone(), tokens[start..at].join(" ")));
    }
    (!sites.is_empty()).then_some(sites)
}

// Whitespace and comments do not separate preprocessing tokens into different
// declarations. Keep their original byte positions while locating the invocation.
#[cfg(feature = "cpp")]
fn skip_trivia(source: &str, mut at: usize) -> Option<usize> {
    loop {
        while source
            .as_bytes()
            .get(at)
            .is_some_and(u8::is_ascii_whitespace)
        {
            at += 1;
        }
        let rest = &source[at..];
        if let Some(comment) = rest.strip_prefix("/*") {
            at += 2 + comment.find("*/")? + 2;
        } else if rest.starts_with("//") {
            at += rest.find('\n').unwrap_or(rest.len());
        } else {
            return Some(at);
        }
    }
}

/// Locate a function-like invocation at a direct member declaration boundary.
/// Its end comes from balanced source tokens, independently of whether the raw
/// grammar merged a following declaration into the same node. This is only a
/// candidate: indexing must prove a complete member inventory before omitting it.
#[cfg(feature = "cpp")]
pub(crate) fn candidate(node: Node<'_>, source: &str) -> Option<(String, types::TextRange)> {
    if node.parent()?.kind() != "field_declaration_list"
        || !matches!(
            node.kind(),
            "field_declaration" | "declaration" | "function_definition" | "ERROR"
        )
    {
        return None;
    }
    let raw = node.utf8_text(source.as_bytes()).ok()?;
    let name_end = raw
        .bytes()
        .position(|b| !b.is_ascii_alphanumeric() && b != b'_')?;
    let name = &raw[..name_end];
    if !identifier(name) {
        return None;
    }
    let open = skip_trivia(raw, name_end)?;
    if raw.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let mut end = crate::cpp_annotations::invocation_end(raw, open)?;
    let after = skip_trivia(raw, end)?;
    if raw.as_bytes().get(after) == Some(&b';') {
        end = after + 1;
    }
    invocation(&raw[..end])?;
    if node
        .parent()?
        .parent()?
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok())
        == Some(name)
        || node
            .child_by_field_name("type")
            .is_some_and(|ty| ty.kind() == "primitive_type")
    {
        return None;
    }
    let text = &raw[..end];
    let lines = text.bytes().filter(|b| *b == b'\n').count();
    let mut range = crate::languages::node_range(node);
    range.end_byte = (node.start_byte() + end).try_into().ok()?;
    range.end_line = range.start_line + u32::try_from(lines).ok()?;
    range.end_column = if lines == 0 {
        range.start_column + u32::try_from(end).ok()?
    } else {
        text.rsplit('\n').next()?.len().try_into().ok()?
    };
    Some((name.into(), range))
}

/// All visible alternatives must have a known declaration inventory. Their
/// union blocks selection of a written member with any potentially added name.
pub fn introduced_names(
    invocation_site: &CppMemberMacro,
    definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
) -> Option<Vec<String>> {
    let mut remaining = MAX_FRAGMENT_BYTES;
    member_names(
        invocation_site,
        definitions,
        &mut HashSet::new(),
        &mut remaining,
    )
}

// Recurse only through complete member-declaration invocations. This does not
// implement arbitrary token-stream rescanning or macro argument prescan.
fn member_names(
    invocation_site: &CppMemberMacro,
    definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
    path: &mut HashSet<String>,
    remaining: &mut usize,
) -> Option<Vec<String>> {
    let (name, arguments, semicolon) = invocation(&invocation_site.text)?;
    if path.len() >= 32 || !path.insert(name.clone()) {
        return None;
    }
    let result = (|| {
        let alternatives = definitions.get(name.as_str())?;
        if alternatives.is_empty() {
            return None;
        }
        let mut names = Vec::new();
        for definition in alternatives {
            // Variadic annotation wrappers do not extend the supported member
            // declaration composition rules.
            if definition.variadic {
                return None;
            }
            let parameters = definition.parameters.as_ref()?;
            let arguments = if parameters.is_empty() && arguments == [Vec::<String>::new()] {
                &[][..]
            } else {
                arguments.as_slice()
            };
            if parameters.len() != arguments.len() {
                return None;
            }
            if arguments.iter().flatten().any(|token| {
                identifier(token)
                    && (definitions.contains_key(token.as_str()) || token.starts_with("__"))
            }) {
                return None;
            }
            let replacement = tokens(definition.replacement.as_ref()?)?;
            let mut expanded: Vec<String> = Vec::new();
            let mut expanded_bytes = 0usize;
            let mut paste = false;
            let mut replacement_tokens = replacement.iter().enumerate();
            while let Some((index, token)) = replacement_tokens.next() {
                if token == "#" {
                    if paste {
                        return None;
                    }
                    let (_, parameter) = replacement_tokens.next()?;
                    let argument = &arguments[parameters.iter().position(|p| p == parameter)?];
                    // For zero or one preprocessing token, stringization does
                    // not require inter-token whitespace facts. Preserve its
                    // spelling and escape literal quotes/backslashes. General
                    // multi-token spelling and argument prescan remain unknown.
                    if argument.len() > 1 {
                        return None;
                    }
                    let spelling = argument.first().map_or("", String::as_str);
                    let literal = format!(
                        "\"{}\"",
                        spelling.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    expanded_bytes = expanded_bytes.checked_add(literal.len() + 1)?;
                    if expanded_bytes > *remaining {
                        return None;
                    }
                    expanded.push(literal);
                    continue;
                }
                if token == "##" {
                    if paste || expanded.last().is_none_or(|left| !identifier(left)) {
                        return None;
                    }
                    paste = true;
                    continue;
                }
                let additions = parameters
                    .iter()
                    .position(|parameter| parameter == token)
                    .map(|position| arguments[position].as_slice())
                    .unwrap_or_else(|| std::slice::from_ref(token));
                // Only paste single ASCII identifier operands from the
                // replacement list. Empty/multi-token arguments, numeric
                // tokens and general rescanning stay unknown. A stringized
                // literal cannot participate in this identifier-only paste.
                if (paste || replacement.get(index + 1).is_some_and(|next| next == "##"))
                    && (additions.len() != 1 || !identifier(&additions[0]))
                {
                    return None;
                }
                for addition in additions {
                    expanded_bytes = expanded_bytes.checked_add(addition.len() + 1)?;
                    if expanded_bytes > *remaining {
                        return None;
                    }
                    if paste {
                        expanded.last_mut()?.push_str(addition);
                    } else {
                        expanded.push(addition.clone());
                    }
                }
                paste = false;
            }
            if paste {
                return None;
            }
            let fragment = expanded.join(" ");
            *remaining = remaining.checked_sub(fragment.len())?;
            names.extend(declaration_names(
                &fragment,
                semicolon,
                invocation_site,
                definitions,
                path,
                remaining,
            )?);
        }
        names.sort();
        names.dedup();
        Some(names)
    })();
    path.remove(&name);
    result
}

#[cfg(not(feature = "cpp"))]
fn declaration_names(
    _fragment: &str,
    _semicolon: bool,
    _site: &CppMemberMacro,
    _definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
    _path: &mut HashSet<String>,
    _remaining: &mut usize,
) -> Option<Vec<String>> {
    None
}

#[cfg(feature = "cpp")]
fn declaration_names(
    fragment: &str,
    semicolon: bool,
    site: &CppMemberMacro,
    definitions: &HashMap<&str, Vec<&CppMacroDefinition>>,
    path: &mut HashSet<String>,
    remaining: &mut usize,
) -> Option<Vec<String>> {
    if fragment.len() > MAX_FRAGMENT_BYTES {
        return None;
    }
    let class_name = site.scope.rsplit("::").next()?;
    if !identifier(class_name) {
        return None;
    }
    // Only a written invocation semicolon can terminate the final generated
    // declaration. Without it, the replacement must be self-terminated.
    // Use the actual class name to parse constructors.
    let terminator = if semicolon { ";" } else { "" };
    let fragment = format!("{fragment}{terminator}");
    if let Some(sites) = invocation_sequence(&fragment, definitions) {
        let mut names = Vec::new();
        for (name, text) in sites {
            names.extend(member_names(
                &CppMemberMacro {
                    scope: site.scope.clone(),
                    name,
                    text,
                    range: site.range,
                },
                definitions,
                path,
                remaining,
            )?);
        }
        return Some(names);
    }
    let source = format!("struct {class_name} {{ {fragment}\n}};");
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(&source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut cursor = root.walk();
    let records: Vec<_> = root.named_children(&mut cursor).collect();
    let [record] = records.as_slice() else {
        return None;
    };
    let body = record.child_by_field_name("body")?;
    if record.kind() != "struct_specifier" || body.end_byte() + 1 != source.len() {
        return None;
    }
    let mut names = Vec::new();
    let mut cursor = body.walk();
    for declaration in body.named_children(&mut cursor) {
        let text = declaration.utf8_text(source.as_bytes()).ok()?;
        if let Some((name, _, _)) = invocation(text)
            && definitions.contains_key(name.as_str())
        {
            names.extend(member_names(
                &CppMemberMacro {
                    scope: site.scope.clone(),
                    name,
                    text: text.into(),
                    range: site.range,
                },
                definitions,
                path,
                remaining,
            )?);
            continue;
        }
        // A macro in a name, type, initializer, function body, or unfinished
        // invocation could change the declaration inventory. Keep it unknown.
        if tokens(text)?.iter().any(|token| {
            identifier(token)
                && (definitions.contains_key(token.as_str()) || token.starts_with("__"))
        }) {
            return None;
        }
        // Access labels affect access checking, not which member names this
        // fragment introduces. Retain names at every access level below;
        // private/protected declarations still participate in name lookup.
        if declaration.kind() == "access_specifier" {
            continue;
        }
        if !matches!(
            declaration.kind(),
            "field_declaration" | "function_definition" | "declaration"
        ) {
            return None;
        }
        // Definitions of nested records/enums can introduce additional names;
        // friends, aliases and most operators are deferred.
        if declaration.child_by_field_name("type").is_some_and(|ty| {
            !matches!(
                ty.kind(),
                "primitive_type" | "type_identifier" | "qualified_identifier" | "template_type"
            )
        }) {
            return None;
        }
        let mut declarator_cursor = declaration.walk();
        let declarators: Vec<_> = declaration
            .children_by_field_name("declarator", &mut declarator_cursor)
            .collect();
        if declarators.is_empty() {
            return None;
        }
        for mut declarator in declarators {
            while !matches!(
                declarator.kind(),
                "identifier" | "field_identifier" | "destructor_name" | "operator_name"
            ) {
                if !matches!(
                    declarator.kind(),
                    "pointer_declarator"
                        | "reference_declarator"
                        | "function_declarator"
                        | "array_declarator"
                        | "init_declarator"
                ) {
                    return None;
                }
                declarator = declarator.child_by_field_name("declarator").or_else(|| {
                    (declarator.kind() == "reference_declarator"
                        && declarator.named_child_count() == 1)
                        .then(|| declarator.named_child(0))
                        .flatten()
                })?;
            }
            let name = declarator
                .utf8_text(source.as_bytes())
                .ok()?
                .split_whitespace()
                .collect::<String>();
            if declarator.kind() == "operator_name" && name != "operator=" {
                return None;
            }
            if declaration.child_by_field_name("type").is_none()
                && name != class_name
                && name != format!("~{class_name}")
            {
                return None;
            }
            names.push(name);
        }
    }
    Some(names)
}
