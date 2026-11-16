//! Base lookup. Primary-template substitution proves name absence; it does not
//! select template members or instantiate method signatures.
use super::*;

/// A template argument retains the scope and position where it was written.
/// Replacing T with its spelling in the template's scope can select another type.
#[derive(Clone)]
struct TypeUse {
    declared: CppDeclaredType,
    scope: String,
    site: ReferenceUse,
    arguments: Vec<TypeUse>,
}

struct NameAbsence<'a> {
    name: &'a str,
    site: &'a ReferenceUse,
    instantiation_files: HashSet<FileId>,
    path: &'a mut HashSet<SymbolId>,
}

fn substitute(
    written: &CppDeclaredType,
    scope: &str,
    site: &ReferenceUse,
    bindings: &HashMap<String, TypeUse>,
) -> Option<TypeUse> {
    if written.pointer || written.reference || written.const_ || written.volatile {
        return None;
    }
    if let Some(argument) = bindings.get(&written.name) {
        return written
            .template_arguments
            .is_empty()
            .then(|| argument.clone());
    }
    // T::Nested and template-template parameters need separate dependent lookup.
    if written
        .name
        .split("::")
        .next()
        .is_some_and(|first| bindings.contains_key(first))
    {
        return None;
    }
    let arguments: Vec<_> = written
        .template_arguments
        .iter()
        .map(|argument| substitute(argument, scope, site, bindings))
        .collect::<Option<_>>()?;
    let mut declared = written.clone();
    declared.template_arguments = arguments
        .iter()
        .map(|argument| argument.declared.clone())
        .collect();
    Some(TypeUse {
        declared,
        scope: scope.into(),
        site: site.clone(),
        arguments,
    })
}

impl TypeIndex {
    pub(super) fn visible_files(&self, file: FileId) -> HashSet<FileId> {
        let mut result = HashSet::new();
        let mut pending = vec![file];
        while let Some(file) = pending.pop() {
            if result.insert(file) {
                pending.extend(
                    self.includes
                        .get(&file)
                        .into_iter()
                        .flatten()
                        .map(|(target, _)| *target),
                );
            }
        }
        result
    }

    /// A definition used to complete a previously bound identity must be
    /// included before the member access. This retains the existing treatment
    /// of nested include files; it does not perform preprocessing.
    pub(super) fn visible_files_at(&self, site: &ReferenceUse) -> HashSet<FileId> {
        let mut result = HashSet::new();
        let mut pending = vec![site.file_id];
        while let Some(file) = pending.pop() {
            if result.insert(file) {
                pending.extend(
                    self.includes
                        .get(&file)
                        .into_iter()
                        .flatten()
                        .filter(|(_, start)| file != site.file_id || *start < site.range.start_byte)
                        .map(|(target, _)| *target),
                );
            }
        }
        result
    }

    pub(super) fn base_record<'a>(
        &'a self,
        record: &SymbolDef,
        base: &types::cpp::CppBaseClass,
        reference: &ReferenceUse,
    ) -> Lookup<(&'a SymbolDef, &'a CppRecordType)> {
        let owner = record
            .qualified_name
            .rsplit_once("::")
            .map_or("", |(owner, _)| owner);
        // Member selection and override checks do not yet instantiate methods.
        // Retaining the base arguments must not silently enable those paths.
        if !base.declared_type.template_arguments.is_empty() {
            return Err(LookupFailure::Type(Box::new(
                types::cpp::CppTypeLookupFailure {
                    kind: types::cpp::CppTypeLookupFailureKind::TemplateUnsupported,
                    name: base.declared_type.name.clone(),
                    scope: owner.into(),
                    file_id: record.file_id,
                    range: base.range,
                    related_declarations: Vec::new(),
                },
            )));
        }
        let ty = &base.declared_type;
        // The base name is looked up at its declaration, not in the caller's
        // namespace or at an unrelated later declaration in the caller file.
        let mut site = reference.clone();
        site.file_id = record.file_id;
        site.range = base.range;
        self.lookup_record(
            ty,
            owner,
            TypeLookup::at(&site, &self.visible_files(record.file_id)),
        )
    }

    /// Search inherited type names without falling into a base's enclosing
    /// namespace. Identical type declarations reached through multiple bases
    /// designate one type; differing sets remain ambiguous without dominance.
    pub(super) fn inherited_type<'a>(
        &'a self,
        record: &SymbolDef,
        record_type: &CppRecordType,
        requested: &CppDeclaredType,
        reference: &ReferenceUse,
        path: &mut TypeLookupPath,
        complete_at: Option<&ReferenceUse>,
    ) -> Lookup<Option<NamedType<'a>>> {
        use types::cpp::{CppTypeLookupFailure, CppTypeLookupFailureKind as Kind};
        let failure = |kind| {
            LookupFailure::Type(Box::new(CppTypeLookupFailure {
                kind,
                name: requested.name.clone(),
                scope: record.qualified_name.clone(),
                file_id: reference.file_id,
                range: reference.range,
                related_declarations: Vec::new(),
            }))
        };
        if self.limited_record(record, record_type) {
            return Err(failure(Kind::LookupRestricted));
        }
        if record_type.template_parameters.is_some() {
            return Err(failure(Kind::TemplateUnsupported));
        }
        let mut found: Option<NamedType<'_>> = None;
        let imported = self.visible_files(reference.file_id);
        for base in record_type
            .bases
            .as_ref()
            .ok_or(LookupFailure::Unspecified)?
        {
            let owner = record
                .qualified_name
                .rsplit_once("::")
                .map_or("", |(scope, _)| scope);
            let site = ReferenceUse {
                file_id: record.file_id,
                range: base.range,
                ..reference.clone()
            };
            let (symbol, ty) = self
                .lookup_type_in(
                    &base.declared_type,
                    owner,
                    TypeLookup::at(&site, &self.visible_files(record.file_id)),
                    path,
                    false,
                )?
                .ok_or(LookupFailure::Unspecified)?
                .record()?;
            let candidate = if let Some(parameters) = &ty.template_parameters {
                // Selecting a nested/injected type of an instantiated template
                // needs an instantiated identity. Reuse the existing bounded
                // substitution only to prove this name absent, as before.
                let name = requested.name.split("::").next().unwrap_or("");
                if self
                    .specialized_templates
                    .get(&symbol.qualified_name)
                    .is_some_and(|files| files.iter().any(|file| imported.contains(file)))
                {
                    return Err(failure(Kind::TemplateUnsupported));
                }
                if symbol.name == name || self.has_member_name(symbol, name) {
                    return Err(failure(Kind::LookupRestricted));
                }
                let use_ = substitute(&base.declared_type, owner, &site, &HashMap::new())
                    .ok_or(LookupFailure::Unspecified)?;
                if parameters.len() != use_.arguments.len() {
                    return Err(LookupFailure::Unspecified);
                }
                let bindings = parameters.iter().cloned().zip(use_.arguments).collect();
                if !self.name_absent_in_bases(
                    symbol,
                    ty,
                    &bindings,
                    &mut NameAbsence {
                        name,
                        site: reference,
                        instantiation_files: imported.clone(),
                        path: &mut HashSet::new(),
                    },
                    path,
                )? {
                    return Err(failure(Kind::LookupRestricted));
                }
                None
            } else {
                self.lookup_type_in(
                    requested,
                    &symbol.qualified_name,
                    TypeLookup {
                        site: reference,
                        imported: &imported,
                        complete_at,
                    },
                    path,
                    true,
                )?
            };
            if let Some(candidate) = candidate {
                if found
                    .as_ref()
                    .is_some_and(|previous| !previous.same_identity(&candidate))
                {
                    return Err(LookupFailure::Type(Box::new(
                        types::cpp::CppTypeLookupFailure {
                            kind: types::cpp::CppTypeLookupFailureKind::DefinitionAmbiguous,
                            name: requested.name.clone(),
                            scope: record.qualified_name.clone(),
                            file_id: reference.file_id,
                            range: reference.range,
                            related_declarations: Vec::new(),
                        },
                    )));
                }
                found = Some(candidate);
            }
        }
        Ok(found)
    }

    pub(super) fn limited_record(&self, record: &SymbolDef, ty: &CppRecordType) -> bool {
        !ty.lookup_supported
            || self
                .lookup_limits
                .get(&record.qualified_name)
                .is_some_and(|files| files.contains(&record.file_id))
    }

    pub(super) fn has_member_name(&self, record: &SymbolDef, name: &str) -> bool {
        self.names
            .get(&format!("{}::{name}", record.qualified_name))
            .into_iter()
            .flatten()
            .any(|(file, _, _)| *file == record.file_id)
            || self.imported_member(record, name)
    }

    fn imported_member(&self, record: &SymbolDef, name: &str) -> bool {
        self.lookup_name_limits
            .get(&format!("{}::{name}", record.qualified_name))
            .is_some_and(|files| files.contains(&record.file_id))
    }

    pub(super) fn member_lookup<'a>(
        &self,
        record: &SymbolDef,
        ty: &CppRecordType,
        reference: &ReferenceUse,
        candidates: &'a [SymbolDef],
        path: &mut HashSet<SymbolId>,
    ) -> Lookup<Vec<&'a SymbolDef>> {
        if self.limited_record(record, ty)
            || self.imported_member(record, &reference.name)
            || !path.insert(record.id)
        {
            return Err(LookupFailure::Unspecified);
        }
        let result = (|| {
            if self.has_member_name(record, &reference.name) {
                // Lookup stops at any own declaration. In particular a field
                // or unsupported callable must not fall back to a base method.
                return Ok(candidates
                    .iter()
                    .filter(|symbol| {
                        symbol.language == Language::Cpp
                            && symbol.qualified_name
                                == format!("{}::{}", record.qualified_name, reference.name)
                            && symbol.container == Some(record.id)
                            && symbol.file_id == record.file_id
                    })
                    .collect());
            }
            let mut found = Vec::new();
            for base in ty.bases.as_ref().ok_or(LookupFailure::Unspecified)? {
                // Virtual-subobject dominance needs a richer lookup set.
                if base.virtual_ {
                    return Err(LookupFailure::Unspecified);
                }
                let (symbol, base_type) = self.base_record(record, base, reference)?;
                let members = self.member_lookup(symbol, base_type, reference, candidates, path)?;
                if !members.is_empty() {
                    if !found.is_empty() {
                        return Err(LookupFailure::Unspecified);
                    }
                    found = members;
                }
            }
            Ok(found)
        })();
        path.remove(&record.id);
        result
    }

    pub(super) fn nonvirtual_named(
        &self,
        record: &SymbolDef,
        ty: &CppRecordType,
        reference: &ReferenceUse,
        candidates: &[SymbolDef],
        path: &mut HashSet<SymbolId>,
    ) -> Lookup<bool> {
        if self.limited_record(record, ty)
            || self.imported_member(record, &reference.name)
            || !path.insert(record.id)
        {
            return Ok(false);
        }
        let result = (|| {
            // Even a hidden virtual in a distant base can make an override
            // virtual without a written virtual/override token on the method.
            for symbol in candidates.iter().filter(|s| {
                s.container == Some(record.id)
                    && s.file_id == record.file_id
                    && matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
            }) {
                let Some(callable) = self.callable(symbol) else {
                    return Ok(false);
                };
                if callable.is_virtual {
                    return Err(LookupFailure::virtual_member(reference, record, symbol));
                }
            }
            let Some(bases) = &ty.bases else {
                return Ok(false);
            };
            let ordinary = (|| {
                for base in bases {
                    let (symbol, ty) = self.base_record(record, base, reference)?;
                    if !self.nonvirtual_named(symbol, ty, reference, candidates, path)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            })();
            if matches!(ordinary, Ok(true)) {
                return Ok(true);
            }
            // No base declaration with this name means no inherited virtual
            // can make the written member an override. Reuse the same bounded
            // substitution, visibility and specialization checks as type-name
            // lookup; absence needs no template method instantiation.
            // Keep ancestors in the cycle guard, but let the shared traversal
            // enter the current record itself.
            let mut absence_path = path.clone();
            absence_path.remove(&record.id);
            match self.inherited_name_absent(
                record,
                ty,
                &reference.name,
                reference,
                &mut absence_path,
            ) {
                // A failed ordinary lookup is not a blocker when the alternate
                // name-absence proof succeeds (e.g. a supported template base).
                Ok(true) => Ok(true),
                Err(failure @ LookupFailure::Type(_)) => Err(failure),
                _ => match ordinary {
                    Err(failure @ LookupFailure::Type(_)) => Err(failure),
                    _ => Ok(false),
                },
            }
        })();
        path.remove(&record.id);
        result
    }

    pub(super) fn inherited_name_absent(
        &self,
        record: &SymbolDef,
        ty: &CppRecordType,
        name: &str,
        reference: &ReferenceUse,
        path: &mut HashSet<SymbolId>,
    ) -> Lookup<bool> {
        self.name_absent_in_bases(
            record,
            ty,
            &HashMap::new(),
            &mut NameAbsence {
                name,
                site: reference,
                instantiation_files: self.visible_files(reference.file_id),
                path,
            },
            &mut TypeLookupPath::new(),
        )
    }

    fn name_absent_in_bases(
        &self,
        record: &SymbolDef,
        ty: &CppRecordType,
        bindings: &HashMap<String, TypeUse>,
        lookup: &mut NameAbsence<'_>,
        type_path: &mut TypeLookupPath,
    ) -> Lookup<bool> {
        if self.limited_record(record, ty)
            || ty
                .template_parameters
                .as_ref()
                .is_some_and(|params| params.len() != bindings.len())
            || !lookup.path.insert(record.id)
        {
            return Ok(false);
        }
        let result = (|| {
            let Some(bases) = &ty.bases else {
                return Ok(false);
            };
            for base in bases {
                let owner = record
                    .qualified_name
                    .rsplit_once("::")
                    .map_or("", |(o, _)| o);
                let site = ReferenceUse {
                    file_id: record.file_id,
                    range: base.range,
                    ..lookup.site.clone()
                };
                let Some(base_use) = substitute(&base.declared_type, owner, &site, bindings) else {
                    return Ok(false);
                };
                let (symbol, base_type) = self
                    .lookup_type_in(
                        &base_use.declared,
                        &base_use.scope,
                        TypeLookup::at(&base_use.site, &self.visible_files(base_use.site.file_id)),
                        type_path,
                        false,
                    )?
                    .ok_or(LookupFailure::Unspecified)?
                    .record()?;
                // A specialization visible at the outer instantiation can change
                // inherited names even if the primary's header cannot see it.
                if self
                    .specialized_templates
                    .get(&symbol.qualified_name)
                    .is_some_and(|files| {
                        files
                            .iter()
                            .any(|file| lookup.instantiation_files.contains(file))
                    })
                    || symbol.name == lookup.name
                    || self.has_member_name(symbol, lookup.name)
                {
                    return Ok(false);
                }
                let parameters = base_type.template_parameters.as_deref().unwrap_or_default();
                if parameters.len() != base_use.arguments.len() {
                    return Ok(false);
                }
                let bindings = parameters.iter().cloned().zip(base_use.arguments).collect();
                // Proving absence needs no virtual-subobject dominance: any
                // own or inherited declaration makes the answer non-absent.
                if !self.name_absent_in_bases(symbol, base_type, &bindings, lookup, type_path)? {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        lookup.path.remove(&record.id);
        result
    }
}
