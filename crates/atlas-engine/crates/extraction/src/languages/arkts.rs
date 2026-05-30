//! ArkTS frontend spec — thin wrapper around TypeScript.
//!
//! ArkTS (HarmonyOS) uses TypeScript-compatible syntax with `.ets`/`.sts` extensions.
//! Delegates all normalization to `TypeScriptFrontendSpec`, only overriding `language()`.
//!
//! ## Recovery Layer
//!
//! ArkTS has one syntax not in TS: the `struct` keyword for declarative UI components.
//! Tree-sitter treats `struct Index { ... }` as an ERROR node because `struct` is not
//! valid TS.  [`ArkTsRecovery`] implements [`RecoverySpec`] to recover these as
//! `SymbolKind::Struct` definitions with `ScopeKind::Struct` scopes.

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, RecoverySpec, ReferenceExtractorSpec,
    ScopeExtractorSpec, SymbolExtractorSpec,
};
use crate::languages::shared::SymbolDefBuilder;
use std::path::Path;
use types::capability::FeatureSupport;
use types::*;

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
    super::typescript::normalize_ts_definition(capture_name, node, source, file_id, Language::ArkTS)
}

fn normalize_arkts_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ReferenceUse> {
    super::typescript::normalize_ts_reference(capture_name, node, source, file_id)
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
    _source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ScopeDef> {
    super::typescript::normalize_ts_scope(capture_name, node, file_id)
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for ArkTsAdapter {
    fn language(&self) -> Language {
        Language::ArkTS
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

impl SymbolExtractorSpec for ArkTsAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/typescript/definitions.scm")
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
        include_str!("../../queries/typescript/references.scm")
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
// ArkTS-specific recovery — struct declarations via ERROR node repair
// ---------------------------------------------------------------------------

/// Recovers ArkTS `struct` declarations from tree-sitter ERROR nodes.
///
/// ArkTS uses `struct` for declarative UI components (e.g., `struct Index { ... }`).
/// Since tree-sitter-typescript does not recognize `struct`, it produces an ERROR node.
/// This recovery walks ERROR nodes, identifies those starting with `"struct "`,
/// and creates proper `SymbolKind::Struct` definitions with `ScopeKind::Struct` scopes.
pub(crate) struct ArkTsRecovery;

impl RecoverySpec for ArkTsRecovery {
    fn recover_definitions(
        &self,
        source: &str,
        tree: &tree_sitter::Tree,
        file_id: FileId,
        symbols: &mut Vec<SymbolDef>,
        scopes: &mut Vec<ScopeDef>,
    ) {
        let root = tree.root_node();
        let source_bytes = source.as_bytes();

        // Walk all nodes looking for ERROR nodes whose text starts with "struct "
        let mut to_visit: Vec<tree_sitter::Node<'_>> = vec![root];

        while let Some(node) = to_visit.pop() {
            if node.kind() == "ERROR" {
                // Check if this ERROR node text starts with "struct "
                if let Ok(text) = node.utf8_text(source_bytes) {
                    if let Some(struct_name) = extract_struct_name(text) {
                        if let Some((def_range, name_range)) =
                            find_struct_range(node, struct_name, source_bytes)
                        {
                            let symbol = build_struct_symbol(
                                file_id,
                                struct_name,
                                def_range,
                                name_range,
                            );
                            let scope = build_struct_scope(file_id, struct_name, def_range, name_range);

                            symbols.push(symbol);
                            scopes.push(scope);
                        }
                    }
                }
            }

            // Push children in reverse so we process in document order
            for i in (0..node.child_count()).rev() {
                if let Some(child) = node.child(i as u32) {
                    to_visit.push(child);
                }
            }
        }
    }

    fn recover_scopes(
        &self,
        _source: &str,
        _tree: &tree_sitter::Tree,
        _file_id: FileId,
        _symbols: &mut Vec<SymbolDef>,
        _scopes: &mut Vec<ScopeDef>,
    ) {
        // v1: No additional scope recovery needed.
        // The struct scope ranges set in recover_definitions() already cover the
        // entire struct body, enabling correct container binding via
        // build_scope_tree() → assign_containers().
        //
        // Reserved for v2: recovery of struct members (methods, @State fields)
        // that tree-sitter may fail to parse inside the struct body.
    }
}

/// Extract the struct name from ERROR node text that starts with "struct ".
///
/// e.g., `"struct Index {"` → `Some("Index")`
///        `"struct\nMyComp\n{\n"` → `Some("MyComp")`
fn extract_struct_name(error_text: &str) -> Option<&str> {
    // Strip "struct" keyword, then skip any whitespace/newlines before the name.
    let after_keyword = error_text.strip_prefix("struct")?;
    let after_keyword = after_keyword.trim_start();
    // Take until `{`, newline, or end of text
    let name = after_keyword
        .split(|c: char| c == '{' || c == '\n' || c == '\r')
        .next()?
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Find the struct's definition range (from `struct` keyword to closing `}`)
/// and the name range (byte offsets for the struct name).
///
/// Uses brace-matching from the opening `{` to find the closing `}`.
fn find_struct_range(
    error_node: tree_sitter::Node,
    struct_name: &str,
    source_bytes: &[u8],
) -> Option<(TextRange, TextRange)> {
    let error_start = error_node.start_byte() as usize;

    // Find the opening `{` in source starting from error_start
    let source_slice = &source_bytes[error_start..];
    let brace_offset = source_slice.iter().position(|&b| b == b'{')?;
    let open_brace_pos = error_start + brace_offset;

    // Brace-match to find closing `}`
    let close_brace_pos = match_brace(source_bytes, open_brace_pos)?;

    // Find struct name position in source
    // The name appears after "struct " and before `{`
    let struct_keyword_pos = error_start; // "struct" starts at error node start
    let name_start = struct_keyword_pos + "struct ".len();
    // Find exact name position by scanning from name_start
    let name_byte_offset = source_bytes[name_start..]
        .windows(struct_name.len())
        .position(|w| w == struct_name.as_bytes())?;
    let name_start_byte = (name_start + name_byte_offset) as u32;
    let name_end_byte = name_start_byte + struct_name.len() as u32;

    let def_range = TextRange {
        start_byte: error_start as u32,
        end_byte: (close_brace_pos + 1) as u32,
        start_line: error_node.start_position().row as u32,
        start_column: error_node.start_position().column as u32,
        // end_line/end_column are approximate — the closing `}` position
        end_line: 0,
        end_column: 0,
    };

    let name_range = TextRange {
        start_byte: name_start_byte,
        end_byte: name_end_byte,
        start_line: 0,
        start_column: 0,
        end_line: 0,
        end_column: 0,
    };

    Some((def_range, name_range))
}

/// Brace-match: given the byte position of an opening `{`, find the matching `}`.
///
/// Handles nested braces by counting depth.
fn match_brace(source_bytes: &[u8], open_pos: usize) -> Option<usize> {
    let mut depth: u32 = 0;
    for (i, &b) in source_bytes[open_pos..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open_pos + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Build a [`SymbolDef`] for a recovered struct.
fn build_struct_symbol(
    file_id: FileId,
    name: &str,
    def_range: TextRange,
    name_range: TextRange,
) -> SymbolDef {
    SymbolDefBuilder::new(
        file_id,
        Language::ArkTS,
        SymbolKind::Struct,
        name.to_string(),
        name.to_string(),
        def_range,
    )
    .name_range(name_range)
    .build()
}

/// Build a [`ScopeDef`] for a recovered struct.
///
/// CRITICAL: scope range MUST include the struct name_range so that
/// `build_scope_tree()` → `assign_containers()` can correctly bind
/// struct members to the struct symbol.
fn build_struct_scope(
    file_id: FileId,
    _name: &str,
    def_range: TextRange,
    _name_range: TextRange,
) -> ScopeDef {
    let scope_id = ScopeId::generate(
        &file_id,
        None::<&ScopeId>,
        ScopeKind::Struct.as_str(),
        def_range.start_byte,
    );
    let scope_name = format!("struct#{}", def_range.start_byte);

    ScopeDef {
        id: scope_id,
        file_id,
        kind: ScopeKind::Struct,
        name: scope_name.clone(),
        scope_path: scope_name,
        parent_id: None,
        range: def_range,
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
        recovery: Box::new(ArkTsRecovery),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_extract_struct_name_simple() {
        assert_eq!(extract_struct_name("struct Index {"), Some("Index"));
    }

    #[test]
    fn test_extract_struct_name_no_brace() {
        assert_eq!(
            extract_struct_name("struct Index "),
            Some("Index")
        );
    }

    #[test]
    fn test_extract_struct_name_multiline() {
        assert_eq!(
            extract_struct_name("struct\nMyComp\n{\n"),
            Some("MyComp")
        );
    }

    #[test]
    fn test_extract_struct_name_not_struct() {
        assert_eq!(extract_struct_name("function foo()"), None);
    }

    #[test]
    fn test_extract_struct_name_empty() {
        assert_eq!(extract_struct_name("struct {"), None);
    }

    #[test]
    fn test_match_brace_simple() {
        let src = b"struct Foo { build() {} }";
        let open = src.iter().position(|&b| b == b'{').unwrap();
        let close = match_brace(src, open);
        assert_eq!(close, Some(src.len() - 1));
    }

    #[test]
    fn test_match_brace_nested() {
        let src = b"{ outer { inner } outer }";
        let close = match_brace(src, 0);
        assert_eq!(close, Some(src.len() - 1));
    }
}
