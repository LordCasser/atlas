//! JavaScript frontend spec — thin wrapper around TypeScript.
//!
//! Uses tree-sitter-typescript grammar and embedded query files.
//! Delegates all normalization to `TypeScriptFrontendSpec`, only overriding `language()`.

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use types::capability::FeatureSupport;
use types::*;
use std::path::Path;

/// JavaScript adapter — delegates to TypeScript internally.
pub(crate) struct JavaScriptAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — shared by all slot trait impls.
// ---------------------------------------------------------------------------

fn normalize_js_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<SymbolDef> {
    super::typescript::normalize_ts_definition(
        capture_name,
        node,
        source,
        file_id,
        Language::JavaScript,
    )
}

fn normalize_js_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ReferenceUse> {
    super::typescript::normalize_ts_reference(capture_name, node, source, file_id)
}

fn normalize_js_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ImportDef> {
    super::typescript::normalize_ts_import(capture_name, node, source, file_id)
}

fn normalize_js_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    _source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<ScopeDef> {
    super::typescript::normalize_ts_scope(capture_name, node, file_id)
}

fn normalize_js_lexical(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> Option<BindingDef> {
    super::typescript::normalize_ts_lexical(capture_name, node, source, file_id)
}

fn normalize_js_dataflow_builder(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
    _file_path: &Path,
) -> (Option<DataNode>, Option<DataFlowEdge>) {
    super::typescript::normalize_ts_dataflow_builder(capture_name, node, source, file_id)
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for JavaScriptAdapter {
    fn language(&self) -> Language {
        Language::JavaScript
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

impl SymbolExtractorSpec for JavaScriptAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/typescript/definitions.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_js_definition(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl ReferenceExtractorSpec for JavaScriptAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/typescript/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_js_reference(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl ImportExtractorSpec for JavaScriptAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/typescript/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_js_import(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl ScopeExtractorSpec for JavaScriptAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/typescript/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_js_scope(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl LexicalBindingSpec for JavaScriptAdapter {
    fn lexical_query(&self) -> &str {
        include_str!("../../queries/typescript/lexical.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["name-based binding (no proper shadowing)"],
        )
    }
    fn normalize(&self, ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<BindingDef> {
        normalize_js_lexical(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

impl DataflowSpec for JavaScriptAdapter {
    fn dataflow_builder_query(&self) -> &str {
        include_str!("../../queries/typescript/dataflow_builder.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported_with_limitations(
            0.55,
            vec!["AST-driven local dataflow with language-specific gaps"],
        )
    }
    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        normalize_js_dataflow_builder(
            &capture.name,
            capture.node,
            ctx.source,
            ctx.file_id,
            ctx.file_path,
        )
    }
}

// ---------------------------------------------------------------------------
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

/// Construct a [`LanguageFrontend`] directly from JavaScript-specific slot
/// implementations — no adapter wrapper needed.
pub(crate) fn javascript_frontend() -> LanguageFrontend {
    use crate::callsite_spec::create_extractor;
    use types::capability::LanguageCapabilityProfile;

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(JavaScriptAdapter),
        symbols: Box::new(JavaScriptAdapter),
        references: Box::new(JavaScriptAdapter),
        imports: Box::new(JavaScriptAdapter),
        scopes: Box::new(JavaScriptAdapter),
        callsites: create_extractor(Language::JavaScript),
        lexical: Box::new(JavaScriptAdapter),
        dataflow: Box::new(JavaScriptAdapter),
        capability: LanguageCapabilityProfile::for_language(Language::JavaScript),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_adapter_metadata() {
        let spec = JavaScriptAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        // Grammar must be valid
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }
}
