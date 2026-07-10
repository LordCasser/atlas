//! ArkTS frontend spec — TypeScript grammar with byte-stable normalization.
//!
//! ArkTS (HarmonyOS) uses TypeScript-compatible syntax with `.ets`/`.sts` extensions.
//! It delegates standard syntax to the TypeScript frontend. Before parsing, ArkTS
//! `struct` declarations are rewritten to the equal-length token `class ` so the
//! fallback grammar can preserve their members and scopes without shifting ranges.
//!
//! ArkUI trailing-block calls such as `Column() { ... }` still produce local ERROR
//! nodes. The ArkTS queries repair the one systematic artifact produced by the TS
//! grammar: nested component calls represented as object-literal methods.

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::make_scope_def_auto_name;
use std::borrow::Cow;
use std::path::Path;
use types::capability::FeatureSupport;
use types::*;

const ARKTS_DEFINITIONS_QUERY: &str = concat!(
    include_str!("../../queries/typescript/definitions.scm"),
    "\n(public_field_definition name: (property_identifier) @definition.field)\n"
);

const ARKTS_REFERENCES_QUERY: &str = concat!(
    include_str!("../../queries/typescript/references.scm"),
    "\n(expression_statement (object (method_definition name: (property_identifier) @reference.call)))\n"
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
    if capture_name == "definition.method" && is_declarative_block_method(node) {
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
    if capture_name == "reference.type"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "class_declaration")
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
    if capture_name == "scope.method" && is_declarative_block_method(node) {
        return None;
    }
    if capture_name == "scope.class" && is_arkts_struct(node, source) {
        let mut range = super::node_range(node);
        let keyword_start = arkts_struct_keyword_start(node, source)?;
        let prefix = &source.as_bytes()[..keyword_start];
        range.start_byte = keyword_start as u32;
        range.start_line = prefix.iter().filter(|byte| **byte == b'\n').count() as u32;
        range.start_column = prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(keyword_start, |newline| keyword_start - newline - 1)
            as u32;
        return Some(make_scope_def_auto_name(file_id, ScopeKind::Struct, range));
    }
    super::typescript::normalize_ts_scope(capture_name, node, file_id)
}

fn is_arkts_struct(node: tree_sitter::Node<'_>, source: &str) -> bool {
    arkts_struct_keyword_start(node, source).is_some()
}

fn arkts_struct_keyword_start(node: tree_sitter::Node<'_>, source: &str) -> Option<usize> {
    let declaration = if node.kind() == "class_declaration" {
        node
    } else {
        let mut current = node;
        loop {
            let Some(parent) = current.parent() else {
                return None;
            };
            if parent.kind() == "class_declaration" {
                break parent;
            }
            current = parent;
        }
    };
    let name = declaration.child_by_field_name("name")?;
    let prefix = source.get(declaration.start_byte()..name.start_byte())?;
    let trimmed = prefix.trim_end();
    let keyword_start = trimmed.len().checked_sub("struct".len())?;
    (trimmed.get(keyword_start..) == Some("struct"))
        .then_some(declaration.start_byte() + keyword_start)
}

fn is_declarative_block_method(node: tree_sitter::Node<'_>) -> bool {
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
        .filter(|parent| parent.kind() == "object")
        .and_then(|object| object.parent())
        .is_some_and(|parent| parent.kind() == "expression_statement")
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
}

impl SymbolExtractorSpec for ArkTsAdapter {
    fn definition_query(&self) -> &str {
        ARKTS_DEFINITIONS_QUERY
    }
    fn manifest_query(&self) -> &str {
        include_str!("../../queries/typescript/manifest.scm")
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
        include_str!("../../queries/typescript/scopes.scm")
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
        let source = r#"@Component
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
        assert!(facts.callsites.iter().all(|callsite| {
            callsite.caller == build.id || callsite.range.start_byte < build.range.start_byte
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
