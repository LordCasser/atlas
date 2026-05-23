//! ArkTS frontend spec — thin wrapper around TypeScript.
//!
//! ArkTS (HarmonyOS) uses TypeScript-compatible syntax with `.ets`/`.sts` extensions.
//! Delegates all normalization to `TypeScriptFrontendSpec`, only overriding `language()`.

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use types::capability::FeatureSupport;
use types::*;
use std::path::Path;

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
        FeatureSupport::supported_with_limitations(0.45, vec!["ArkTS via TS grammar fallback — lexical bindings may miss ArkTS-specific constructs"])
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        crate::languages::typescript::normalize_ts_lexical(&capture.name, capture.node, ctx.source, ctx.file_id)
    }
}

impl DataflowSpec for ArkTsAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/typescript/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(0.45, vec!["ArkTS via TS grammar fallback — dataflow may miss ArkTS-specific constructs"])
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> (Option<DataNode>, Option<DataFlowEdge>) {
        crate::languages::typescript::normalize_ts_dataflow_builder(&capture.name, capture.node, ctx.source, ctx.file_id)
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
    fn test_arkts_scope_query_parses() {
        let spec = ArkTsAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }
}
