//! Bash frontend spec (slot-based).
//!
//! Provides query-driven extraction for Bash scripts.
//! Bash is an **opt-in-only** scripting language with severely limited support:
//! functions and variables as symbols, commands as call references, source/.
//! builtins as best-effort imports, and file/function level scopes only.
//!
//! No block scoping, no dataflow, no lexical bindings.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::SymbolDefBuilder;
use types::capability::FeatureSupport;
use types::*;

// ---------------------------------------------------------------------------
// Adapter struct
// ---------------------------------------------------------------------------

/// Bash frontend spec.
pub(crate) struct BashAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers
// ---------------------------------------------------------------------------

fn normalize_bash_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = bash_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    Some(
        SymbolDefBuilder::new(
            file_id,
            Language::Bash,
            kind,
            name.clone(),
            name,
            range,
        )
        .build(),
    )
}

fn normalize_bash_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = bash_reference_kind(capture_name)?;
    let text = node_text(node, source)?;
    let name = text.clone();
    let range = node_range(node);

    let ref_id = ReferenceId::generate(
        &file_id,
        None::<&SymbolId>,
        range.start_byte,
        range.end_byte,
        &text,
        kind,
    );

    Some(ReferenceUse {
        id: ref_id,
        file_id,
        source_symbol: None,
        scope_id: None,
        kind,
        text,
        name,
        receiver: None,
        arity: None,
        range,
        resolved: None,
        binding_id: None,
    })
}

fn normalize_bash_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, imported_name) = bash_import_info(capture_name, node, source)?;
    let range = node_range(node);
    let is_relative = !module.starts_with('/');

    let import_id = ImportId::generate(
        &file_id,
        kind.as_str(),
        &module,
        Some(imported_name.as_str()),
        range.start_byte,
    );

    Some(ImportDef {
        id: import_id,
        file_id,
        kind,
        module,
        imported_name: imported_name.clone(),
        local_name: Some(imported_name),
        is_wildcard: false,
        is_relative,
        range,
        alias: None,
    })
}

fn normalize_bash_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = match capture_name {
        "scope.file" => ScopeKind::File,
        "scope.function" => ScopeKind::Function,
        _ => return None,
    };
    let range = node_range(node);
    let name = format!("{:?}#{}", kind, range.start_byte);
    let scope_path = name.clone();

    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);

    Some(ScopeDef {
        id: scope_id,
        file_id,
        kind,
        name,
        scope_path,
        parent_id: None,
        range,
    })
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for BashAdapter {
    fn language(&self) -> Language {
        Language::Bash
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_bash::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for BashAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/bash/definitions.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_bash_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for BashAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/bash/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_bash_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for BashAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/bash/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_bash_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for BashAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/bash/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_bash_scope(&capture.name, capture.node, _ctx.file_id)
    }
}

impl LexicalBindingSpec for BashAdapter {
    fn lexical_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported("Bash does not support lexical binding extraction")
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, _capture: Capture<'_>) -> Option<BindingDef> {
        None
    }
}

impl DataflowSpec for BashAdapter {
    fn dataflow_builder_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported("Bash does not support dataflow extraction")
    }
    fn normalize(
        &self,
        _ctx: NormalizeCtx<'_>,
        _capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>) {
        (None, None)
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub(crate) fn bash_frontend() -> LanguageFrontend {
    let lang = Language::Bash;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(BashAdapter),
        symbols: Box::new(BashAdapter),
        references: Box::new(BashAdapter),
        imports: Box::new(BashAdapter),
        scopes: Box::new(BashAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(BashAdapter),
        dataflow: Box::new(BashAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map capture name to SymbolKind.
fn bash_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.function" => Some(SymbolKind::Function),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

/// Map capture name to ReferenceKind.
fn bash_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        _ => None,
    }
}

/// Extract import info from capture.
fn bash_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let text = node_text(node, source)?;
            let cleaned = text.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((ImportKind::Include, cleaned.clone(), cleaned))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_metadata() {
        let spec = BashAdapter;
        let ts_lang = spec.tree_sitter_language();
        assert!(!spec.definition_query().is_empty());
        tree_sitter::Parser::new().set_language(&ts_lang).unwrap();
    }

    #[test]
    fn test_def_query_parses() {
        let spec = BashAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(
            query.is_ok(),
            "definition query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_ref_query_parses() {
        let spec = BashAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(
            query.is_ok(),
            "reference query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_import_query_parses() {
        let spec = BashAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.import_query());
        assert!(
            query.is_ok(),
            "import query must compile: {:?}",
            query.err()
        );
    }

    #[test]
    fn test_scope_query_parses() {
        let spec = BashAdapter;
        let lang = spec.tree_sitter_language();
        let query = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(query.is_ok(), "scope query must compile: {:?}", query.err());
    }
}
