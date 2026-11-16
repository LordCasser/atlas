//! Shared read-only function ownership and existing lazy extraction.
use super::*;
struct Cancellation<'a>(&'a dyn Fn() -> bool);
impl extraction::CancelCheck for Cancellation<'_> {
    fn is_cancelled(&self) -> bool {
        (self.0)()
    }
}

impl Investigation<'_> {
    pub(super) fn function_owner(
        &self,
        at: &ContextLocation,
        parsed: &ParsedSource,
    ) -> anyhow::Result<Option<SymbolDef>> {
        let syntax =
            extraction::cpp_expressions::expression_function(parsed.tree.root_node(), at.range);
        let parameter = parsed
            .tree
            .root_node()
            .descendant_for_byte_range(at.range.start_byte as usize, at.range.end_byte as usize)
            .is_some_and(|node| {
                std::iter::successors(Some(node), |n| n.parent())
                    .take_while(|n| {
                        !matches!(n.kind(), "function_definition" | "lambda_expression")
                    })
                    .any(|n| {
                        matches!(
                            n.kind(),
                            "parameter_declaration" | "optional_parameter_declaration"
                        )
                    })
            });
        let owners: Vec<_> =
            self.store
                .find_symbols_by_file(&at.file_id)?
                .into_iter()
                .filter(|symbol| {
                    matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                        && syntax.is_some_and(|syntax| {
                            symbol.range == cpp::range(syntax)
                                && (parameter
                                    || syntax.child_by_field_name("body").is_some_and(|body| {
                                        body.start_byte() <= at.range.start_byte as usize
                                            && at.range.end_byte as usize <= body.end_byte()
                                    }))
                        })
                        && !parsed.cpp.as_ref().is_some_and(|facts| {
                            facts.unverified_callable_scopes.contains(&symbol.id)
                        })
                })
                .collect();
        Ok(if owners.len() == 1 {
            owners.into_iter().next()
        } else {
            None
        })
    }
    pub(super) fn function_facts(
        &self,
        owner: &SymbolDef,
        parsed: &ParsedSource,
        include_parameter_outputs: bool,
    ) -> anyhow::Result<FileFacts> {
        self.check()?;
        let file = self
            .store
            .get_file(&owner.file_id)?
            .ok_or_else(|| anyhow::anyhow!("source file metadata missing"))?;
        let unit = AnalysisUnit::from_function(file.file_id, owner.id, owner.range);
        let window = LazyWindow {
            seed_unit: unit.clone(),
            units: vec![unit.clone()],
            variable_focus: None,
            truncated: false,
            units_built: 0,
            units_cached: 0,
            units_pending: 0,
            pending_job_ids: vec![],
            quality: None,
            capability_mask: FactCoverage::default(),
        };
        let (annotations, members) = parsed
            .cpp
            .as_ref()
            .map(|f| {
                (
                    f.normalized_annotations.clone(),
                    f.normalized_member_macros.clone(),
                )
            })
            .unwrap_or_default();
        let frontend = extraction::cpp_annotations::frontend(annotations, members)
            .ok_or_else(|| anyhow::anyhow!("C++ grammar unavailable"))?;
        extraction::extract_file_with_mode(
            &frontend,
            file.file_id,
            Path::new(&file.path),
            &parsed.source,
            &file.content_hash,
            extraction::ExtractionMode::LazyDataflow {
                include_parameter_outputs,
                window,
                callsites: self.store.find_callsites_by_file(&file.file_id)?,
            },
            &Cancellation(self.canceled),
        )
    }
}
