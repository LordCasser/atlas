//! ArkTS frontend spec — TypeScript grammar with byte-stable normalization.
//!
//! ArkTS (HarmonyOS) uses TypeScript-compatible syntax with `.ets`/`.sts` extensions.
//! It delegates standard syntax to the TypeScript frontend. Before parsing, ArkTS
//! `struct` declarations are rewritten to the equal-length token `class ` so the
//! fallback grammar can preserve their members and scopes without shifting ranges.
//!
//! ArkUI trailing-block calls such as `Column() { ... }` still produce local ERROR
//! nodes. The primary tree keeps expression facts; a byte-stable declaration-only
//! recovery tree prevents those errors from inventing methods or swallowing later
//! declarations.

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::node_text;
use crate::languages::shared::{make_reference_use, make_scope_def_auto_name};
use std::borrow::Cow;
use std::path::Path;
use types::capability::FeatureSupport;
use types::*;

const ARKTS_DEFINITIONS_QUERY: &str = concat!(
    include_str!("../../queries/typescript/definitions.scm"),
    "\n(class name: (type_identifier) @definition.class)\n",
    "\n(public_field_definition name: (property_identifier) @definition.field)\n"
);

const ARKTS_MANIFEST_QUERY: &str = concat!(
    include_str!("../../queries/typescript/manifest.scm"),
    "\n(class name: (type_identifier) @definition.class)\n"
);

const ARKTS_REFERENCES_QUERY: &str = concat!(
    include_str!("../../queries/typescript/references.scm"),
    "\n(expression_statement (object (method_definition name: (property_identifier) @reference.call)))\n",
    "\n(decorator (identifier) @reference.decorator)\n",
    "\n(decorator (call_expression function: (identifier) @reference.decorator))\n"
);

const ARKTS_SCOPES_QUERY: &str = concat!(
    include_str!("../../queries/typescript/scopes.scm"),
    "\n(class) @scope.class\n"
);

/// ArkTS frontend slots backed by the shared TypeScript-family grammar.
pub(crate) struct ArkTsAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_arkts_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<SymbolDef> {
    if capture_name == "definition.method" && is_nested_arkts_method_definition(node) {
        return None;
    }

    let mut symbol = super::typescript::normalize_ts_definition(
        capture_name,
        node,
        source,
        file_id,
        Language::ArkTS,
    )?;

    if symbol.kind == SymbolKind::Class && is_arkts_struct(node, source) {
        symbol.kind = SymbolKind::Struct;
        symbol.id = SymbolId::generate(
            &file_id,
            Language::ArkTS.as_str(),
            &symbol.qualified_name,
            SymbolKind::Struct.as_str(),
            None::<&str>,
        );
    }
    Some(symbol)
}

fn normalize_arkts_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ReferenceUse> {
    if capture_name == "reference.decorator" {
        let name = node_text(node, source)?;
        let decorator = std::iter::successors(Some(node), |current| current.parent())
            .find(|current| current.kind() == "decorator")?;
        // `text` preserves decorator arguments (e.g. "Extend(Button)"), while
        // `name` is the bare identifier used for exact decorator lookup.
        let decorator_text = node_text(decorator, source)?;
        let text = decorator_text.strip_prefix('@').unwrap_or(&decorator_text);
        return Some(make_reference_use(
            file_id,
            ReferenceKind::Decoration,
            text.to_string(),
            name,
            super::node_range(decorator),
        ));
    }
    if capture_name == "reference.type"
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "class_declaration" | "class"))
    {
        return None;
    }
    let mut reference =
        super::typescript::normalize_ts_reference(capture_name, node, source, file_id)?;
    if let Some(member) = node
        .parent()
        .filter(|parent| parent.kind() == "member_expression")
    {
        reference.receiver = member
            .child_by_field_name("object")
            .and_then(|object| object.utf8_text(source.as_bytes()).ok())
            .map(str::to_string);
    }
    Some(reference)
}

/// Source-scan fallback for ArkTS decorator references.
///
/// In complex ArkUI files, cascading parse errors cause the tree-sitter
/// TypeScript grammar to swallow `@Decorator` nodes into giant
/// `call_expression` error-recovery nodes. The query-based extraction then
/// misses these decorators entirely.
///
/// This function scans the raw source for `@Identifier` and
/// `@Identifier(...)` patterns and adds decoration references for any that
/// are not already present in `references` (deduplicated by byte range).
/// Parser-recognized strings, templates, regexes, and comments are excluded
/// from the scan and from parameter-list delimiter matching.
pub(crate) fn arkts_decorator_fallback(
    source: &str,
    root: tree_sitter::Node<'_>,
    file_id: FileId,
    references: &mut Vec<ReferenceUse>,
) {
    use types::TextRange;

    let bytes = source.as_bytes();

    // Collect existing decoration reference ranges for deduplication.
    let existing_ranges: std::collections::HashSet<(u32, u32)> = references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Decoration)
        .map(|r| (r.range.start_byte, r.range.end_byte))
        .collect();

    let ignored = non_code_ranges(root);
    let mut ignored_index = 0;
    let mut i = 0;

    while i < bytes.len() {
        while ignored.get(ignored_index).is_some_and(|(_, end)| *end <= i) {
            ignored_index += 1;
        }
        if let Some((start, end)) = ignored.get(ignored_index).copied()
            && i >= start
            && i < end
        {
            i = end;
            ignored_index += 1;
            continue;
        }

        if bytes[i] == b'@' {
            // The character after `@` must be an identifier start.
            if i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_alphabetic() {
                i += 1;
                continue;
            }

            let at_byte = i;

            // Extract the identifier name.
            let name_start = i + 1;
            let mut name_end = name_start;
            while name_end < bytes.len()
                && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
            {
                name_end += 1;
            }
            let name = &source[name_start..name_end];

            // Skip `$r` builtin (not a decorator).
            if name == "r" || name.is_empty() {
                i = name_end;
                continue;
            }

            // Determine the full decorator end byte.
            let mut end_byte = name_end;
            let mut text = name.to_string();

            // Skip whitespace after the identifier name.
            let mut scan = name_end;
            while scan < bytes.len() && bytes[scan].is_ascii_whitespace() {
                scan += 1;
            }

            if scan < bytes.len() && bytes[scan] == b'(' {
                // Parameterized decorator: find matching closing paren.
                let mut depth = 1i32;
                let mut p = scan + 1;
                let mut argument_ignored_index = ignored.partition_point(|(_, end)| *end <= p);
                while p < bytes.len() && depth > 0 {
                    if let Some((start, end)) = ignored.get(argument_ignored_index).copied()
                        && p >= start
                    {
                        p = end.max(p + 1);
                        argument_ignored_index += 1;
                        continue;
                    }
                    match bytes[p] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    p += 1;
                }
                if depth == 0 {
                    end_byte = p;
                    text = source[name_start..end_byte].to_string();
                } else {
                    // Unbalanced; skip.
                    i = name_end;
                    continue;
                }
            }

            // Check for deduplication.
            let range_key = (at_byte as u32, end_byte as u32);
            if existing_ranges.contains(&range_key) {
                i = end_byte;
                continue;
            }

            // Build the range for this decorator reference.
            let start_line = source[..at_byte].matches('\n').count() as u32;
            let last_newline = source[..at_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let start_column = (at_byte - last_newline) as u32;

            let end_line = source[..end_byte].matches('\n').count() as u32;
            let last_newline_end = source[..end_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let end_column = (end_byte - last_newline_end) as u32;

            references.push(make_reference_use(
                file_id,
                ReferenceKind::Decoration,
                text,
                name.to_string(),
                TextRange {
                    start_byte: at_byte as u32,
                    end_byte: end_byte as u32,
                    start_line,
                    start_column,
                    end_line,
                    end_column,
                },
            ));

            i = end_byte;
            continue;
        }

        i += 1;
    }
}

fn normalize_arkts_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ImportDef> {
    super::typescript::normalize_ts_import(capture_name, node, source, file_id)
}

fn normalize_arkts_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ScopeDef> {
    if capture_name == "scope.method" && is_nested_arkts_method_definition(node) {
        return None;
    }
    if capture_name == "scope.class" && is_arkts_struct(node, source) {
        let range = arkts_struct_range(node, source)?;
        return Some(make_scope_def_auto_name(file_id, ScopeKind::Struct, range));
    }
    super::typescript::normalize_ts_scope(capture_name, node, file_id)
}

fn is_arkts_struct(node: tree_sitter::Node<'_>, source: &str) -> bool {
    arkts_struct_keyword_start(node, source).is_some()
}

fn enclosing_arkts_class(mut node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    loop {
        if matches!(node.kind(), "class_declaration" | "class") {
            return Some(node);
        }
        node = node.parent()?;
    }
}

fn arkts_struct_keyword_start(node: tree_sitter::Node<'_>, source: &str) -> Option<usize> {
    let declaration = enclosing_arkts_class(node)?;
    let name = declaration.child_by_field_name("name")?;
    let prefix = source.get(declaration.start_byte()..name.start_byte())?;
    let trimmed = prefix.trim_end();
    let keyword_start = trimmed.len().checked_sub("struct".len())?;
    (trimmed.get(keyword_start..) == Some("struct"))
        .then_some(declaration.start_byte() + keyword_start)
}

fn arkts_struct_range(node: tree_sitter::Node<'_>, source: &str) -> Option<TextRange> {
    let declaration = enclosing_arkts_class(node)?;
    let start = arkts_struct_keyword_start(declaration, source)?;
    let body_start = declaration.child_by_field_name("body")?.start_byte();
    let end = matching_brace_end(declaration, source, body_start)?;
    let bytes = source.as_bytes();
    let start_prefix = &bytes[..start];
    let end_prefix = &bytes[..end];
    Some(TextRange {
        start_byte: start as u32,
        end_byte: end as u32,
        start_line: start_prefix.iter().filter(|byte| **byte == b'\n').count() as u32,
        start_column: start_prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(start, |newline| start - newline - 1) as u32,
        end_line: end_prefix.iter().filter(|byte| **byte == b'\n').count() as u32,
        end_column: end_prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(end, |newline| end - newline - 1) as u32,
    })
}

fn matching_brace_end(
    declaration: tree_sitter::Node<'_>,
    source: &str,
    open: usize,
) -> Option<usize> {
    let mut root = declaration;
    while let Some(parent) = root.parent() {
        root = parent;
    }

    let ignored = non_code_ranges(root);

    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut ignored_index = ignored.partition_point(|(_, end)| *end <= open);
    let mut depth = 0_u32;
    let mut offset = open;
    while offset < bytes.len() {
        if let Some((start, end)) = ignored.get(ignored_index).copied()
            && offset >= start
        {
            offset = end.max(offset + 1);
            ignored_index += 1;
            continue;
        }
        match bytes[offset] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
        offset += 1;
    }
    None
}

fn non_code_ranges(root: tree_sitter::Node<'_>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "comment" | "string" | "template_string" | "regex"
        ) {
            ranges.push((node.start_byte(), node.end_byte()));
            continue;
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    ranges.sort_unstable();
    ranges
}

fn is_nested_arkts_method_definition(node: tree_sitter::Node<'_>) -> bool {
    let method = if node.kind() == "method_definition" {
        node
    } else {
        match node.parent() {
            Some(parent) if parent.kind() == "method_definition" => parent,
            _ => return false,
        }
    };
    method
        .parent()
        .is_none_or(|parent| parent.kind() != "class_body")
}

fn normalize_struct_keywords(source: &str) -> Cow<'_, str> {
    let bytes = source.as_bytes();
    let mut normalized: Option<Vec<u8>> = None;
    let mut offset = 0;

    while let Some(relative) = source[offset..].find("struct") {
        let start = offset + relative;
        let end = start + "struct".len();
        let before_is_ident = source[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'));
        let after_is_space = bytes.get(end).is_some_and(u8::is_ascii_whitespace);
        let mut name_start = end;
        while bytes.get(name_start).is_some_and(u8::is_ascii_whitespace) {
            name_start += 1;
        }
        let has_name = source[name_start..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || matches!(ch, '_' | '$'));

        if !before_is_ident && after_is_space && has_name {
            let output = normalized.get_or_insert_with(|| bytes.to_vec());
            output[start..end].copy_from_slice(b"class ");
        }
        offset = end;
    }

    match normalized {
        Some(bytes) => Cow::Owned(String::from_utf8(bytes).expect("ASCII replacement is UTF-8")),
        None => Cow::Borrowed(source),
    }
}

/// Maximum number of replacement-and-reparse iterations. Each iteration is a
/// full AST traversal plus a full reparse; converging in practice takes 1-2
/// rounds. The hard limit prevents pathological inputs from hanging the
/// indexer.
const MAX_DECLARATION_RECOVERY_ITERATIONS: usize = 3;

fn recover_arkts_declaration_source(
    parser_source: &str,
    primary_root: tree_sitter::Node<'_>,
) -> Option<String> {
    // TS reads `Component(args) { ... }` as a nested method. In the declaration
    // view only, replace the complete fake method header with an equal-width,
    // valid `if(1)` header so the surrounding class and later declarations close.
    let mut output = parser_source.as_bytes().to_vec();
    let primary_replacements = nested_method_header_ranges(primary_root);
    if primary_replacements.is_empty() {
        return None;
    }

    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;

    let primary_has_class = has_class_declaration(primary_root);
    let mut best_output: Option<Vec<u8>> = None;
    let mut replacements = primary_replacements;

    for _ in 0..MAX_DECLARATION_RECOVERY_ITERATIONS {
        for (start, end) in replacements.drain(..) {
            if output[start..start + 5]
                .iter()
                .any(|byte| matches!(*byte, b'\n' | b'\r'))
            {
                continue;
            }
            for byte in &mut output[start..end] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
            output[start..start + 5].copy_from_slice(b"if(1)");
        }

        let tree = parser.parse(&output, None)?;
        let root = tree.root_node();

        // Only accept a recovery iteration if it preserves (or creates)
        // a class_declaration node. If the `if` replacement destroys the
        // class_declaration (which happens when the ArkUI DSL is too
        // complex for the `if` trick), the primary tree is strictly
        // better for declaration extraction.
        if has_class_declaration(root) {
            best_output = Some(output.clone());
        }

        replacements = nested_method_header_ranges(root);
        if replacements.is_empty() {
            break;
        }
    }

    // Only use recovered source if it actually preserves a class_declaration.
    // Otherwise the primary tree (which at least has a partial class_declaration)
    // is strictly better for declaration extraction.
    match best_output {
        Some(bytes) => {
            let recovered = String::from_utf8(bytes).ok()?;
            // Safety check: recovery must preserve byte offsets.
            if recovered.len() == parser_source.len() {
                Some(recovered)
            } else {
                None
            }
        }
        None => {
            // Recovery diverged: no iteration preserved class_declaration.
            // If the primary tree HAS a class_declaration, use it directly.
            // Otherwise (primary also lacks class_declaration), fall back to
            // the last recovery attempt anyway - it can't be worse.
            if primary_has_class {
                None
            } else {
                String::from_utf8(output).ok()
            }
        }
    }
}

/// Check whether a tree root contains a `class_declaration` node
/// (directly or inside an `export_statement`).
fn has_class_declaration(root: tree_sitter::Node<'_>) -> bool {
    fn find_class(node: tree_sitter::Node<'_>) -> bool {
        if node.kind() == "class_declaration" {
            return true;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if find_class(child) {
                return true;
            }
        }
        false
    }
    find_class(root)
}

fn nested_method_header_ranges(root: tree_sitter::Node<'_>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "method_definition"
            && node
                .parent()
                .is_none_or(|parent| parent.kind() != "class_body")
            && let (Some(name), Some(parameters)) = (
                node.child_by_field_name("name")
                    .filter(|name| name.kind() == "property_identifier"),
                node.child_by_field_name("parameters"),
            )
        {
            let start = name.start_byte();
            let end = parameters.end_byte();
            if end.saturating_sub(start) >= 5 {
                ranges.push((start, end));
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    ranges.sort_unstable();
    ranges.dedup();
    ranges
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for ArkTsAdapter {
    fn language(&self) -> Language {
        Language::ArkTS
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
    fn parser_source<'a>(&self, source: &'a str) -> Cow<'a, str> {
        normalize_struct_keywords(source)
    }
    fn declaration_recovery_source(
        &self,
        parser_source: &str,
        primary_root: tree_sitter::Node<'_>,
    ) -> Option<String> {
        recover_arkts_declaration_source(parser_source, primary_root)
    }
}

impl SymbolExtractorSpec for ArkTsAdapter {
    fn definition_query(&self) -> &str {
        ARKTS_DEFINITIONS_QUERY
    }
    fn manifest_query(&self) -> &str {
        ARKTS_MANIFEST_QUERY
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_arkts_definition(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl ReferenceExtractorSpec for ArkTsAdapter {
    fn reference_query(&self) -> &str {
        ARKTS_REFERENCES_QUERY
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_arkts_reference(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl ImportExtractorSpec for ArkTsAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/typescript/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_arkts_import(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl ScopeExtractorSpec for ArkTsAdapter {
    fn scope_query(&self) -> &str {
        ARKTS_SCOPES_QUERY
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_arkts_scope(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl LexicalBindingSpec for ArkTsAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/typescript/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.45,
            vec![
                "ArkTS via TS grammar fallback — lexical bindings may miss ArkTS-specific constructs",
            ],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        crate::languages::typescript::normalize_ts_lexical(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
        )
    }
}

impl DataflowSpec for ArkTsAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/typescript/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.45,
            vec!["ArkTS via TS grammar fallback — dataflow may miss ArkTS-specific constructs"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        crate::languages::typescript::normalize_ts_dataflow_builder(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
        )
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

/// Construct a [`LanguageFrontend`] directly from ArkTS-specific slot
/// implementations — no adapter wrapper needed.
pub(crate) fn arkts_frontend() -> LanguageFrontend {
    use crate::callsite_spec::create_extractor;
    use types::capability::LanguageCapabilityProfile;

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(ArkTsAdapter),
        symbols: Box::new(ArkTsAdapter),
        references: Box::new(ArkTsAdapter),
        imports: Box::new(ArkTsAdapter),
        scopes: Box::new(ArkTsAdapter),
        callsites: create_extractor(Language::ArkTS),
        lexical: Box::new(ArkTsAdapter),
        dataflow: Box::new(ArkTsAdapter),
        capability: LanguageCapabilityProfile::for_language(Language::ArkTS),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarative_struct_preserves_members_and_ui_call_ownership() {
        let source = r#"@Component({ freezeWhenInactive: true })
struct MainPage {
  @StorageLink('webUrl') webUrl: string = '';

  build() {
    Row() {
      Column() {
        Web({ src: this.webUrl })
      }
    }
  }
}"#;
        let file_id = FileId::generate("MainPage.ets");
        let frontend = arkts_frontend();
        let facts = crate::extract_file_with_mode(
            &frontend,
            file_id,
            Path::new("MainPage.ets"),
            source,
            "probe",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        assert_eq!(facts.file.status, ParseStatus::Partial);
        let struct_symbol = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "MainPage")
            .unwrap();
        assert_eq!(struct_symbol.kind, SymbolKind::Struct);
        assert_eq!(
            facts
                .symbols
                .iter()
                .filter(|symbol| symbol.name == "MainPage")
                .count(),
            1
        );
        assert_eq!(
            &source[struct_symbol.range.start_byte as usize..struct_symbol.range.end_byte as usize],
            source,
            "struct range must include decorators and the complete declaration"
        );
        assert_eq!(
            struct_symbol.range.end_line,
            source.lines().count() as u32 - 1
        );
        assert_eq!(
            struct_symbol.signature.as_deref(),
            Some("@Component({ freezeWhenInactive: true })")
        );

        let field = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "webUrl")
            .unwrap();
        assert_eq!(field.kind, SymbolKind::Field);
        assert_eq!(
            field.signature.as_deref(),
            Some("@StorageLink('webUrl') : string")
        );
        assert_eq!(
            field.container,
            Some(struct_symbol.id),
            "field={field:#?}\nscopes={:#?}",
            facts.scopes
        );

        let build = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "build")
            .unwrap();
        assert_eq!(build.kind, SymbolKind::Method);
        assert_eq!(build.container, Some(struct_symbol.id));
        assert!(!facts.symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Method && matches!(symbol.name.as_str(), "Row" | "Column")
        }));

        for component in ["Row", "Column", "Web"] {
            let reference = facts
                .references
                .iter()
                .find(|reference| {
                    reference.kind == ReferenceKind::Call && reference.name == component
                })
                .unwrap_or_else(|| panic!("missing UI call reference for {component}"));
            assert_eq!(reference.source_symbol, Some(build.id));
        }
        assert!(!facts.references.iter().any(|reference| {
            reference.kind == ReferenceKind::Call && reference.name == "build"
        }));
        for decorator in ["Component", "StorageLink"] {
            assert!(facts.references.iter().any(|reference| {
                reference.kind == ReferenceKind::Decoration && reference.name == decorator
            }));
        }
        assert!(facts.callsites.iter().all(|callsite| {
            callsite.caller == build.id || callsite.range.start_byte < build.range.start_byte
        }));
    }

    #[test]
    fn ts_compatible_declarations_fill_existing_arkts_ir() {
        let source = r#"export abstract class BaseVM<T extends BaseState> {
  protected state: T;
  public abstract sendEvent(event: BaseEvent): void;
}

export interface ResponseData<T> {
  currentPage: number;
  refresh(force: boolean): Promise<T>;
}

export enum LoadingStatus {
  IDLE = 'idle',
  OFF,
}

export async function load(): Promise<void> {}
"#;
        let frontend = arkts_frontend();
        let facts = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("declarations.ets"),
            Path::new("declarations.ets"),
            source,
            "declarations",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();

        assert_eq!(facts.file.status, ParseStatus::Success);
        for (qualified_name, kind) in [
            ("BaseVM", SymbolKind::Class),
            ("BaseVM.state", SymbolKind::Field),
            ("BaseVM.sendEvent", SymbolKind::Method),
            ("ResponseData", SymbolKind::Interface),
            ("ResponseData.currentPage", SymbolKind::Property),
            ("ResponseData.refresh", SymbolKind::Method),
            ("LoadingStatus", SymbolKind::Enum),
            ("LoadingStatus.IDLE", SymbolKind::EnumMember),
            ("LoadingStatus.OFF", SymbolKind::EnumMember),
        ] {
            assert!(
                facts.symbols.iter().any(|symbol| {
                    symbol.qualified_name == qualified_name && symbol.kind == kind
                }),
                "missing {kind:?} {qualified_name}"
            );
        }
        assert!(
            facts
                .symbols
                .iter()
                .find(|symbol| symbol.name == "load")
                .is_some_and(|symbol| symbol.async_)
        );

        let manifest = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("declarations.ets"),
            Path::new("declarations.ets"),
            source,
            "declarations-manifest",
            crate::ExtractionMode::Manifest,
            &(),
        )
        .unwrap();
        assert!(
            manifest
                .symbols
                .iter()
                .any(|symbol| symbol.name == "BaseVM" && symbol.kind == SymbolKind::Class)
        );
        assert!(!manifest.symbols.iter().any(|symbol| {
            matches!(symbol.kind, SymbolKind::Property | SymbolKind::EnumMember)
        }));
    }

    #[test]
    fn struct_normalization_is_byte_stable_and_token_bounded() {
        let source = "struct MainPage {}\nstruct 页面 {}\nconst restructure = 'struct';";
        let normalized = normalize_struct_keywords(source);
        assert_eq!(normalized.len(), source.len());
        assert_eq!(
            normalized,
            "class  MainPage {}\nclass  页面 {}\nconst restructure = 'struct';"
        );
    }

    #[test]
    fn class_expression_fallback_preserves_struct_range_and_container() {
        let source = r#"@Component({ freezeWhenInactive: true })
struct MainPage {
  private marker: string = '}';
  private template: string = `}`;
  // A brace in a comment must not terminate the struct: }
  build() {
    if (useNewUi()) {
      HdsNavDestination() {
        Text('new')
      }
      .height('100%')
    } else {
      NavDestination() {
        Text('old')
      }
      .height('100%')
    }
  }
}"#;
        let frontend = arkts_frontend();
        let parser_source = frontend.parser.parser_source(source);
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(parser_source.as_bytes(), None).unwrap();
        let name_start = source.find("MainPage {").unwrap();
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(name_start, name_start + "MainPage".len())
            .unwrap();
        let mut ancestors = Vec::new();
        loop {
            ancestors.push(node.kind().to_string());
            let Some(parent) = node.parent() else {
                break;
            };
            node = parent;
        }
        assert!(ancestors.iter().any(|kind| kind == "class"));

        let facts = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("MainPage.ets"),
            Path::new("MainPage.ets"),
            source,
            "probe",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let main_page = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "MainPage" && symbol.kind == SymbolKind::Struct)
            .unwrap();
        assert_eq!(
            facts
                .symbols
                .iter()
                .filter(|symbol| symbol.name == "MainPage")
                .count(),
            1
        );
        assert_eq!(main_page.range.end_line, source.lines().count() as u32 - 1);
        assert_eq!(
            &source[main_page.range.end_byte as usize - 1..main_page.range.end_byte as usize],
            "}"
        );
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.name == "build"
                && symbol.kind == SymbolKind::Method
                && symbol.container == Some(main_page.id)
        }));

        let manifest = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("MainPage.ets"),
            Path::new("MainPage.ets"),
            source,
            "manifest-probe",
            crate::ExtractionMode::Manifest,
            &(),
        )
        .unwrap();
        assert!(
            manifest
                .symbols
                .iter()
                .any(|symbol| { symbol.name == "MainPage" && symbol.kind == SymbolKind::Struct })
        );
    }

    #[test]
    fn declaration_recovery_restores_post_build_styles_and_extend_function() {
        let source = r#"@Component
struct Card {
  build() {
    if (ready) {
      Row() {
        Text('ready')
      }
      .height('100%')
    } else {
      Column() {
        Text('idle')
      }
      .height('100%')
    }
  }

  @Styles
  pressedStyle() {
    .backgroundColor(Color.Transparent)
  }
}

@Extend(Button)
function cardButtonStyle(color: ResourceColor) {
  .fontColor(color)
  .width('100%')
}
"#;
        let frontend = arkts_frontend();
        let parser_source = frontend.parser.parser_source(source);
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let primary_tree = parser.parse(parser_source.as_bytes(), None).unwrap();
        assert!(!nested_method_header_ranges(primary_tree.root_node()).is_empty());

        let facts = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("Card.ets"),
            Path::new("Card.ets"),
            source,
            "declaration-recovery",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let card = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Card")
            .unwrap();
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.qualified_name == "Card.pressedStyle"
                && symbol.kind == SymbolKind::Method
                && symbol.container == Some(card.id)
        }));
        let card_button_style = facts
            .symbols
            .iter()
            .find(|symbol| {
                symbol.qualified_name == "cardButtonStyle" && symbol.kind == SymbolKind::Function
            })
            .expect("cardButtonStyle function");
        assert_eq!(
            &source[card_button_style.range.start_byte as usize
                ..card_button_style.range.end_byte as usize],
            "@Extend(Button)\nfunction cardButtonStyle(color: ResourceColor) {\n  .fontColor(color)\n  .width('100%')\n}"
        );
        assert!(!facts.symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Method
                && matches!(symbol.name.as_str(), "Row" | "Column" | "Text")
        }));
        for decorator in ["Styles", "Extend"] {
            assert!(facts.references.iter().any(|reference| {
                reference.kind == ReferenceKind::Decoration && reference.name == decorator
            }));
        }

        let manifest = crate::extract_file_with_mode(
            &frontend,
            FileId::generate("Card.ets"),
            Path::new("Card.ets"),
            source,
            "declaration-recovery-manifest",
            crate::ExtractionMode::Manifest,
            &(),
        )
        .unwrap();
        assert!(
            manifest
                .symbols
                .iter()
                .any(|symbol| { symbol.name == "Card" && symbol.kind == SymbolKind::Struct })
        );
        let manifest_style = manifest
            .symbols
            .iter()
            .find(|symbol| symbol.name == "cardButtonStyle" && symbol.kind == SymbolKind::Function)
            .expect("manifest cardButtonStyle function");
        assert_eq!(
            &source
                [manifest_style.range.start_byte as usize..manifest_style.range.end_byte as usize],
            "@Extend(Button)\nfunction cardButtonStyle(color: ResourceColor) {\n  .fontColor(color)\n  .width('100%')\n}"
        );
        assert!(
            !manifest
                .symbols
                .iter()
                .any(|symbol| symbol.name == "pressedStyle")
        );
    }

    #[test]
    fn test_arkts_adapter_metadata() {
        let spec = ArkTsAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn declaration_recovery_preserves_mixed_class_struct_and_following_function() {
        let source = r#"class Helper {
  ready(): boolean { return true; }
}

@Component
struct MixedPage {
  build() {
    Column() {
      Text('ready')
    }
    .width('100%')
  }
}

@Styles
function pageStyle() {
  .height('100%')
}
"#;
        let facts = extract_single(source, "MixedPage.ets");
        assert!(
            facts
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Helper" && symbol.kind == SymbolKind::Class)
        );
        assert!(
            facts
                .symbols
                .iter()
                .any(|symbol| symbol.name == "MixedPage" && symbol.kind == SymbolKind::Struct)
        );
        assert!(
            facts
                .symbols
                .iter()
                .any(|symbol| symbol.name == "pageStyle" && symbol.kind == SymbolKind::Function)
        );
    }

    #[test]
    fn declaration_recovery_preserves_navigation_component_and_following_builder() {
        let source = r#"@Component
struct SettingView {
  pathStack: NavPathStack = new NavPathStack();

  build() {
    Navigation(this.pathStack) {
      List() {
        ListItemGroup() {
          ListItem() {
            HmosListItem({
              title: $r('app.string.about')
            })
          }
        }
        .padding({ left: $r('sys.float.padding_level2') })
      }
      .height('100%')
    }
    .hideBackButton(true)
    .title($r('app.string.setting'))
  }
}

@Builder
export function SettingViewBuilder() {
  SettingView()
}
"#;
        let facts = extract_single(source, "SettingView.ets");
        let setting_view = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "SettingView" && symbol.kind == SymbolKind::Struct)
            .unwrap_or_else(|| {
                panic!(
                    "declarative component must survive recovery; symbols={:#?}; diagnostics={:#?}",
                    facts.symbols, facts.diagnostics
                )
            });
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.qualified_name == "SettingView.pathStack"
                && symbol.kind == SymbolKind::Field
                && symbol.container == Some(setting_view.id)
        }));
        assert!(!facts.symbols.iter().any(|symbol| {
            symbol.name == "pathStack"
                && (symbol.qualified_name == "pathStack" || symbol.container.is_none())
        }));
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.qualified_name == "SettingView.build"
                && symbol.kind == SymbolKind::Method
                && symbol.container == Some(setting_view.id)
        }));
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.name == "SettingViewBuilder" && symbol.kind == SymbolKind::Function
        }));
    }

    #[test]
    fn test_arkts_def_query_parses() {
        let spec = ArkTsAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(query.is_ok(), "definition query must compile");
    }

    #[test]
    fn test_arkts_manifest_query_parses() {
        let spec = ArkTsAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.manifest_query());
        assert!(
            query.is_ok(),
            "manifest query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_arkts_scope_query_parses() {
        let spec = ArkTsAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }

    #[test]
    fn test_arkts_reference_query_parses() {
        let spec = ArkTsAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(
            query.is_ok(),
            "reference query must compile: {:?}",
            query.err()
        );
    }

    // ── Decorator and signature contract tests ─────────────────────────────

    fn extract_single(source: &str, path: &str) -> crate::languages::FileFacts {
        let frontend = arkts_frontend();
        crate::extract_file_with_mode(
            &frontend,
            FileId::generate(path),
            Path::new(path),
            source,
            "sig-test",
            crate::ExtractionMode::Full,
            &(),
        )
        .unwrap()
    }

    #[test]
    fn decorated_struct_preserves_decorator_signature() {
        let source = "@Component\nstruct Foo {\n  build() {}\n}\n";
        let facts = extract_single(source, "Foo.ets");
        let foo = facts
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Struct)
            .unwrap();
        assert_eq!(foo.signature.as_deref(), Some("@Component"));
        let decoration = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Decoration && r.name == "Component")
            .unwrap();
        assert!(foo.range.start_byte <= decoration.range.start_byte);
        assert!(foo.range.end_byte >= decoration.range.end_byte);
    }

    #[test]
    fn stacked_decorators_all_extend_the_struct_range() {
        let source = "@Entry\n@Component\nstruct Foo {\n  build() {}\n}\n";
        let facts = extract_single(source, "Foo.ets");
        let foo = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Foo" && symbol.kind == SymbolKind::Struct)
            .unwrap();
        assert_eq!(foo.signature.as_deref(), Some("@Entry @Component"));
        let decorations = facts
            .references
            .iter()
            .filter(|reference| reference.kind == ReferenceKind::Decoration)
            .collect::<Vec<_>>();
        assert_eq!(decorations.len(), 2);
        assert!(decorations.iter().all(|decoration| {
            foo.range.start_byte <= decoration.range.start_byte
                && foo.range.end_byte >= decoration.range.end_byte
        }));
    }

    #[test]
    fn field_signature_preserves_decorator_and_type() {
        let source =
            "@Component\nstruct Widget {\n  @Prop mediaSrc: ResourceStr = '';\n  build() {}\n}\n";
        let facts = extract_single(source, "Widget.ets");
        let media_src = facts
            .symbols
            .iter()
            .find(|s| s.name == "mediaSrc" && s.kind == SymbolKind::Field)
            .unwrap();
        assert_eq!(media_src.signature.as_deref(), Some("@Prop : ResourceStr"));
        let decoration = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Decoration && r.name == "Prop")
            .unwrap();
        assert!(media_src.range.start_byte <= decoration.range.start_byte);
        assert!(media_src.range.end_byte >= decoration.range.end_byte);
    }

    #[test]
    fn resolution_symbols_preserves_decorator_signatures_without_references() {
        let source =
            "@Component\nstruct Widget {\n  @Prop mediaSrc: ResourceStr = '';\n  build() {}\n}\n";
        let path = "Widget.ets";
        let facts = crate::extract_file_with_mode(
            &arkts_frontend(),
            FileId::generate(path),
            Path::new(path),
            source,
            "sig-test",
            crate::ExtractionMode::ResolutionSymbols,
            &(),
        )
        .unwrap();

        let widget = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "Widget" && symbol.kind == SymbolKind::Struct)
            .unwrap();
        let media_src = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "mediaSrc" && symbol.kind == SymbolKind::Field)
            .unwrap();
        assert_eq!(widget.signature.as_deref(), Some("@Component"));
        assert_eq!(media_src.signature.as_deref(), Some("@Prop : ResourceStr"));
        assert!(facts.references.is_empty());
    }

    #[test]
    fn async_function_preserves_async_in_signature_and_flag() {
        let source = "async function load(): Promise<void> {}\n";
        let facts = extract_single(source, "load.ets");
        let load = facts
            .symbols
            .iter()
            .find(|s| s.name == "load" && s.kind == SymbolKind::Function)
            .unwrap();
        assert_eq!(
            load.signature.as_deref(),
            Some("async (): Promise<void>"),
            "ArkTS detail signatures preserve async in the declaration shape"
        );
        assert!(load.async_, "async_ boolean flag must still be set");
    }

    #[test]
    fn async_method_preserves_async_in_signature_and_flag() {
        let source = "class Service {\n  async fetch(url: string): Promise<string> {\n    return '';\n  }\n}\n";
        let facts = extract_single(source, "Service.ets");
        let fetch = facts
            .symbols
            .iter()
            .find(|s| s.name == "fetch" && s.kind == SymbolKind::Method)
            .unwrap();
        assert_eq!(
            fetch.signature.as_deref(),
            Some("async (url: string): Promise<string>"),
            "ArkTS detail signatures preserve async in the declaration shape"
        );
        assert!(fetch.async_);
    }

    #[test]
    fn decorated_method_signature_contains_only_callable_shape() {
        let source = "@Component\nstruct Card {\n  @Builder buildCard(label: string) {}\n}\n";
        let facts = extract_single(source, "Card.ets");
        let build_card = facts
            .symbols
            .iter()
            .find(|s| s.name == "buildCard" && s.kind == SymbolKind::Method)
            .unwrap();
        assert_eq!(
            build_card.signature.as_deref(),
            Some("(label: string)"),
            "decorator metadata must not be duplicated in the callable signature"
        );
    }

    fn fallback_references(source: &str) -> Vec<ReferenceUse> {
        let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut references = Vec::new();
        arkts_decorator_fallback(
            source,
            tree.root_node(),
            FileId::generate("fallback.ets"),
            &mut references,
        );
        references
    }

    #[test]
    fn decorator_fallback_skips_regex_literals() {
        let references = fallback_references("const pattern = /@Component/;\nclass Real {}\n");
        assert!(references.is_empty());
    }

    #[test]
    fn decorator_fallback_balances_arguments_around_string_parens() {
        let source = "@Extend(Button, { label: \")\" })\nfunction style() {}\n";
        let references = fallback_references(source);
        let decoration = references
            .iter()
            .find(|r| r.kind == ReferenceKind::Decoration && r.name == "Extend")
            .unwrap();
        assert_eq!(decoration.text, "Extend(Button, { label: \")\" })");
        assert_eq!(
            &source[decoration.range.start_byte as usize..decoration.range.end_byte as usize],
            "@Extend(Button, { label: \")\" })"
        );
    }

    #[test]
    fn decorator_fallback_does_not_skip_adjacent_decorator_after_arguments() {
        let references = fallback_references("@One(\")\")@Two\nclass Target {}\n");
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>(),
            vec!["One", "Two"]
        );
    }
}
