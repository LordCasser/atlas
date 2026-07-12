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

/// ArkTS adapter — delegates to TypeScript internally.
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
        let text = node_text(node, source)?;
        let decorator = std::iter::successors(Some(node), |current| current.parent())
            .find(|current| current.kind() == "decorator")?;
        return Some(make_reference_use(
            file_id,
            ReferenceKind::Decoration,
            text.clone(),
            text,
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

    let mut ignored = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "comment" | "string" | "template_string" | "regex"
        ) {
            ignored.push((node.start_byte(), node.end_byte()));
            continue;
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    ignored.sort_unstable();

    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut ignored_index = ignored.partition_point(|(_, end)| *end <= open);
    let mut depth = 0_u32;
    let mut offset = open;
    while offset < bytes.len() {
        if let Some((start, end)) = ignored.get(ignored_index).copied() {
            if offset >= start {
                offset = end.max(offset + 1);
                ignored_index += 1;
                continue;
            }
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

fn recover_arkts_declaration_source(
    parser_source: &str,
    primary_root: tree_sitter::Node<'_>,
) -> Option<String> {
    // TS reads `Component(args) { ... }` as a nested method. In the declaration
    // view only, an equal-width `if` token preserves its condition/body shape
    // while allowing the surrounding class and following declarations to close.
    let mut output = parser_source.as_bytes().to_vec();
    let mut replacements = nested_method_name_ranges(primary_root);
    if replacements.is_empty() {
        return None;
    }

    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;

    loop {
        for (start, end) in replacements.drain(..) {
            output[start] = b'i';
            output[start + 1] = b'f';
            output[start + 2..end].fill(b' ');
        }

        let tree = parser.parse(&output, None)?;
        replacements = nested_method_name_ranges(tree.root_node());
        if replacements.is_empty() {
            break;
        }
    }

    String::from_utf8(output).ok()
}

fn nested_method_name_ranges(root: tree_sitter::Node<'_>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "method_definition"
            && node
                .parent()
                .is_none_or(|parent| parent.kind() != "class_body")
        {
            if let Some(name) = node
                .child_by_field_name("name")
                .filter(|name| name.kind() == "property_identifier")
                .filter(|name| name.end_byte() - name.start_byte() >= 2)
            {
                ranges.push((name.start_byte(), name.end_byte()));
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

        let field = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "webUrl")
            .unwrap();
        assert_eq!(field.kind, SymbolKind::Field);
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
        assert!(!nested_method_name_ranges(primary_tree.root_node()).is_empty());

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
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.qualified_name == "cardButtonStyle" && symbol.kind == SymbolKind::Function
        }));
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
        assert!(manifest.symbols.iter().any(|symbol| {
            symbol.name == "cardButtonStyle" && symbol.kind == SymbolKind::Function
        }));
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
}
