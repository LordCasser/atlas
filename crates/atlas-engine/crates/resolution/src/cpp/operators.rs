//! Member access through an actual, visible operator-> declaration.
use super::*;

impl TypeIndex {
    pub(super) fn arrow_record<'a>(
        &'a self,
        ty: &CppDeclaredType,
        scope: &str,
        lookup: TypeLookup<'_>,
        implicit: &mut Vec<ResolvedTarget>,
    ) -> Lookup<(&'a SymbolDef, &'a CppRecordType)> {
        let TypeLookup {
            site,
            imported,
            complete_at,
        } = lookup;
        let (record, record_type) = self.lookup_record(ty, scope, lookup)?;
        // Dependent bases and inherited operator overload sets need additional
        // substitution/lookup. Do not select a superficially matching operator.
        if self.limited_record(record, record_type)
            || !record_type
                .bases
                .as_ref()
                .ok_or(LookupFailure::Unspecified)?
                .is_empty()
            || self
                .lookup_limits
                .get(&record.qualified_name)
                .is_some_and(|files| files.contains(&record.file_id))
        {
            return Err(LookupFailure::Unspecified);
        }
        let visible: Vec<_> = self
            .operators
            .iter()
            .filter(|symbol| {
                symbol.container == Some(record.id) && symbol.file_id == record.file_id
            })
            .collect();
        if visible.is_empty()
            || visible.iter().any(|symbol| {
                self.callable(symbol).is_none_or(|callable| {
                    symbol.static_
                        || callable.is_virtual
                        || !callable.parameter_types.is_empty()
                        || !matches!(callable.qualifiers.as_str(), "" | "const")
                })
            })
        {
            return Err(LookupFailure::Unspecified);
        }
        let visible: Vec<_> = visible
            .into_iter()
            .filter(|symbol| {
                !ty.const_
                    || self
                        .callable(symbol)
                        .is_some_and(|c| c.qualifiers == "const")
            })
            .collect();
        let qname = format!("{}::operator->", record.qualified_name);
        let mut target = choose_target(site, &qname, &visible, &self.operators, 0, self)?;
        let selected = visible
            .iter()
            .find(|symbol| symbol.id == target.symbol_id)
            .copied()
            .or_else(|| visible.first().copied())
            .ok_or(LookupFailure::Unspecified)?;
        target.strategy = ResolutionStrategy::ImplicitOperator;
        implicit.push(target);

        let written = self
            .callable(selected)
            .ok_or(LookupFailure::Unspecified)?
            .return_type
            .as_ref()
            .ok_or(LookupFailure::Unspecified)?;
        if !written.pointer
            || written.reference
            || written.volatile
            || written.const_
            || !written.template_arguments.is_empty()
        {
            return Err(LookupFailure::Unspecified);
        }
        let mut pointee = written.clone();
        pointee.pointer = false;
        let (lookup_scope, lookup_site, lookup_files);
        if let Some(position) = record_type
            .template_parameters
            .as_ref()
            .and_then(|parameters| parameters.iter().position(|name| *name == written.name))
        {
            // Use the declared parameter name, never a convention that the
            // first template argument must be the object reached by operator->.
            pointee = ty
                .template_arguments
                .get(position)
                .ok_or(LookupFailure::Unspecified)?
                .clone();
            lookup_scope = scope.to_string();
            lookup_site = site.clone();
            lookup_files = imported.clone();
        } else {
            // A fixed return class belongs to the operator's declaration scope.
            lookup_scope = record.qualified_name.clone();
            lookup_site = ReferenceUse {
                file_id: selected.file_id,
                range: selected.name_range,
                ..site.clone()
            };
            lookup_files = self.visible_files(selected.file_id);
        }
        if pointee.pointer
            || pointee.reference
            || pointee.const_
            || pointee.volatile
            || !pointee.template_arguments.is_empty()
        {
            return Err(LookupFailure::Unspecified);
        }
        self.lookup_record(
            &pointee,
            &lookup_scope,
            TypeLookup {
                site: &lookup_site,
                imported: &lookup_files,
                complete_at,
            },
        )
    }
}
