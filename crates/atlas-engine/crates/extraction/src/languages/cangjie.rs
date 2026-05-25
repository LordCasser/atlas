//! Cangjie frontend spec (slot-based).
//!
//! Provides query-driven extraction for Cangjie source files.

use crate::languages::{node_range, node_text};

use crate::frontend::{
    Capture, DataflowSpec, FrontendParts, ImportExtractorSpec, LanguageFrontend,
    LexicalBindingSpec, NormalizeCtx, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
    SymbolExtractorSpec,
};
use crate::languages::shared::SymbolDefBuilder;
use types::capability::FeatureSupport;
use types::*;

/// Cangjie frontend spec.
pub(crate) struct CangjieAdapter;

// ---------------------------------------------------------------------------
// Private normalize helpers — single source of truth for both the legacy
// slot trait impls only.
// ---------------------------------------------------------------------------

fn normalize_cangjie_definition(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<SymbolDef> {
    let kind = cj_definition_kind(capture_name)?;
    let name = node_text(node, source)?;
    let range = node_range(node);

    let qualified_name = qualified_name_from_node_cj("", &name, node, source);

    Some(
        SymbolDefBuilder::new(
            file_id,
            Language::Cangjie,
            kind,
            name,
            qualified_name,
            range,
        )
        .build(),
    )
}

fn normalize_cangjie_reference(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ReferenceUse> {
    let kind = cj_reference_kind(capture_name)?;
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

    // source_symbol is resolved by SemanticBinder after extraction.
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

fn normalize_cangjie_import(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ImportDef> {
    let (kind, module, _imported_name) = cj_import_info(capture_name, node, source)?;
    let range = node_range(node);

    let import_id = ImportId::generate(
        &file_id,
        kind.as_str(),
        &module,
        None::<&str>,
        range.start_byte,
    );

    Some(ImportDef {
        id: import_id,
        file_id,
        kind,
        module,
        imported_name: String::new(),
        local_name: None,
        is_wildcard: false,
        is_relative: false,
        range,
        alias: None,
    })
}

fn normalize_cangjie_scope(
    capture_name: &str,
    node: tree_sitter::Node,
    source: &str,
    file_id: FileId,
) -> Option<ScopeDef> {
    let kind = cj_scope_kind(capture_name)?;
    let name = match kind {
        ScopeKind::File => String::new(),
        _ => node_text(node, source).unwrap_or_default(),
    };
    let range = node_range(node);
    let scope_id = ScopeId::generate(&file_id, None::<&ScopeId>, kind.as_str(), range.start_byte);

    Some(ScopeDef {
        id: scope_id,
        file_id,
        kind,
        name,
        scope_path: String::new(),
        parent_id: None,
        range,
    })
}

// ── Slot trait implementations ──────────────────────────────────────────

impl ParserSpec for CangjieAdapter {
    fn language(&self) -> Language {
        Language::Cangjie
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cangjie::LANGUAGE.into()
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
}

impl SymbolExtractorSpec for CangjieAdapter {
    fn definition_query(&self) -> &str {
        include_str!("../../queries/cangjie/definitions.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<SymbolDef> {
        normalize_cangjie_definition(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ReferenceExtractorSpec for CangjieAdapter {
    fn reference_query(&self) -> &str {
        include_str!("../../queries/cangjie/references.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ReferenceUse> {
        normalize_cangjie_reference(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ImportExtractorSpec for CangjieAdapter {
    fn import_query(&self) -> &str {
        include_str!("../../queries/cangjie/imports.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ImportDef> {
        normalize_cangjie_import(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl ScopeExtractorSpec for CangjieAdapter {
    fn scope_query(&self) -> &str {
        include_str!("../../queries/cangjie/scopes.scm")
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::supported()
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, capture: Capture<'_>) -> Option<ScopeDef> {
        normalize_cangjie_scope(&capture.name, capture.node, _ctx.source, _ctx.file_id)
    }
}

impl LexicalBindingSpec for CangjieAdapter {
    fn lexical_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported("Cangjie does not support lexical binding extraction")
    }
    fn normalize(&self, _ctx: NormalizeCtx<'_>, _capture: Capture<'_>) -> Option<BindingDef> {
        None
    }
}

impl DataflowSpec for CangjieAdapter {
    fn dataflow_builder_query(&self) -> &str {
        ""
    }
    fn capability(&self) -> FeatureSupport {
        FeatureSupport::unsupported("Cangjie does not support dataflow extraction")
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
// Factory — direct slot construction, no adapter wrapper needed.
// ---------------------------------------------------------------------------

pub(crate) fn cangjie_frontend() -> LanguageFrontend {
    let lang = Language::Cangjie;
    let callsite_extractor = crate::callsite_spec::create_extractor(lang);
    let cap = LanguageCapabilityProfile::for_language(lang);

    LanguageFrontend::from_parts(FrontendParts {
        parser: Box::new(CangjieAdapter),
        symbols: Box::new(CangjieAdapter),
        references: Box::new(CangjieAdapter),
        imports: Box::new(CangjieAdapter),
        scopes: Box::new(CangjieAdapter),
        callsites: callsite_extractor,
        lexical: Box::new(CangjieAdapter),
        dataflow: Box::new(CangjieAdapter),
        capability: cap,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a qualified name using `::` separators (Cangjie convention).
fn qualified_name_from_node_cj(
    prefix: &str,
    name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> String {
    let mut parts = vec![name.to_string()];
    // Start from parent to avoid re-adding the immediate container's name
    let mut current = node.parent().unwrap_or(node);

    while let Some(parent) = current.parent() {
        if parent.kind() == "classDefinition" {
            if let Some(child) = parent.child_by_field_name("className") {
                if let Ok(class_name) = child.utf8_text(source.as_bytes()) {
                    parts.push(class_name.to_string());
                }
            }
        }
        current = parent;
    }

    parts.reverse();
    if prefix.is_empty() {
        parts.join("::")
    } else {
        format!("{}::{}", prefix, parts.join("::"))
    }
}

fn cj_definition_kind(capture: &str) -> Option<SymbolKind> {
    match capture {
        "definition.class" => Some(SymbolKind::Class),
        "definition.interface" => Some(SymbolKind::Interface),
        "definition.enum" => Some(SymbolKind::Enum),
        "definition.function" => Some(SymbolKind::Function),
        "definition.variable" => Some(SymbolKind::Variable),
        _ => None,
    }
}

fn cj_reference_kind(capture: &str) -> Option<ReferenceKind> {
    match capture {
        "reference.call" => Some(ReferenceKind::Call),
        "reference.field" => Some(ReferenceKind::FieldAccess),
        "reference.type" => Some(ReferenceKind::TypeReference),
        _ => None,
    }
}

fn cj_import_info(
    capture: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<(ImportKind, String, String)> {
    match capture {
        "import.module" => {
            let module_path = node_text(node, source)?;
            Some((ImportKind::Import, module_path.to_string(), String::new()))
        }
        _ => None,
    }
}

fn cj_scope_kind(capture: &str) -> Option<ScopeKind> {
    match capture {
        "scope.file" => Some(ScopeKind::File),
        "scope.class" => Some(ScopeKind::Class),
        "scope.interface" => Some(ScopeKind::Class),
        "scope.function" => Some(ScopeKind::Function),
        "scope.block" => Some(ScopeKind::Block),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cj_adapter_metadata() {
        let spec = CangjieAdapter;
        assert!(!spec.definition_query().is_empty());
        assert!(!spec.reference_query().is_empty());
        assert!(!spec.import_query().is_empty());
        assert!(!spec.scope_query().is_empty());
    }

    #[test]
    fn test_cj_queries_parse() {
        let spec = CangjieAdapter;
        let lang = spec.tree_sitter_language();

        // tree-sitter 0.26+ supports language ABI versions 13-15,
        // making Cangjie (ABI 15) fully compatible.
        let def_q = tree_sitter::Query::new(&lang, spec.definition_query());
        assert!(def_q.is_ok(), "definitions query: {:?}", def_q.err());

        let ref_q = tree_sitter::Query::new(&lang, spec.reference_query());
        assert!(ref_q.is_ok(), "references query: {:?}", ref_q.err());

        let imp_q = tree_sitter::Query::new(&lang, spec.import_query());
        assert!(imp_q.is_ok(), "imports query: {:?}", imp_q.err());

        let sc_q = tree_sitter::Query::new(&lang, spec.scope_query());
        assert!(sc_q.is_ok(), "scopes query: {:?}", sc_q.err());
    }

    #[test]
    fn test_cj_definition_kind_mapping() {
        assert_eq!(
            cj_definition_kind("definition.class"),
            Some(SymbolKind::Class)
        );
        assert_eq!(
            cj_definition_kind("definition.function"),
            Some(SymbolKind::Function)
        );
        assert_eq!(cj_definition_kind("unknown"), None);
    }

    #[test]
    fn test_cj_reference_kind_mapping() {
        assert_eq!(
            cj_reference_kind("reference.call"),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            cj_reference_kind("reference.field"),
            Some(ReferenceKind::FieldAccess)
        );
        assert_eq!(cj_reference_kind("unknown"), None);
    }

    #[test]
    fn test_cj_import_info_mapping() {
        // import_info requires a real tree-sitter Node (from a parse tree)
        // — tests are deferred to E2E fixture-based tests.
    }
}
