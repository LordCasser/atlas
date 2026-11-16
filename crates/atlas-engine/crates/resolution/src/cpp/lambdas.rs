//! Direct closure invocation from an expression or a known local closure type.
use super::*;

impl TypeIndex {
    /// Field lookup depends on an enclosing object. A local receiver instead
    /// uses its own binding/capture facts and never needs a captured this.
    pub(super) fn captured_field_object(
        &self,
        reference: &ReferenceUse,
        context: &ResolutionContext,
        unqualified_name: Option<&str>,
    ) -> Lookup<()> {
        let file = self
            .files
            .get(&reference.file_id)
            .ok_or(LookupFailure::Unspecified)?;
        crate::cpp_captures::field_access(file, reference, unqualified_name, |id| {
            context.symbols_by_id.get(&id).map(|symbol| symbol.static_)
        })
        .ok_or(LookupFailure::Unspecified)
    }
}

pub(super) fn resolve(
    reference: &ReferenceUse,
    context: &ResolutionContext,
    types: Option<&TypeIndex>,
) -> Option<Lookup<ResolvedTarget>> {
    let file = types?.files.get(&reference.file_id)?;
    let lambda = file.lambda_captures.iter().find(|lambda| {
        if let Some(binding) = reference.binding_id {
            return lambda.binding_id == Some(binding);
        }
        lambda
            .symbol_id
            .and_then(|id| context.symbols_by_id.get(&id))
            .is_some_and(|symbol| {
                reference.name == symbol.name && reference.range == symbol.name_range
            })
    })?;
    Some((|| {
        let target = lambda.symbol_id.ok_or(LookupFailure::Unspecified)?;
        // Local aliases/captures, generic closures and argument applicability
        // need additional facts. Preserve their actual callsites as unknown.
        if reference.arity != Some(0)
            || lambda.parameter_count != Some(0)
            || lambda.captures.is_none()
            || file.unverified_callable_scopes.contains(&target)
            || (reference.binding_id.is_some()
                && (reference.source_symbol != lambda.enclosing_symbol
                    || (lambda.binding_const && lambda.mutable_)))
        {
            return Err(LookupFailure::Unspecified);
        }
        if !context.symbols_by_id.contains_key(&target) {
            return Err(LookupFailure::Unspecified);
        }
        Ok(ResolvedTarget {
            symbol_id: target,
            confidence: Confidence::certain(),
            strategy: ResolutionStrategy::ExactMatch,
            provenance: Provenance::TreeSitter,
        })
    })())
}
