//! Fully written type arguments on supported primary function templates.
//! Name lookup is shared with ordinary calls. Unknown deduction, constraints,
//! specializations and competing same-arity templates remain unresolved.
use super::*;

impl TypeIndex {
    pub(crate) fn template_call(
        &self,
        reference: &ReferenceUse,
    ) -> Option<&types::cpp::CppTemplateCall> {
        self.files
            .get(&reference.file_id)?
            .template_calls
            .iter()
            .find(|c| c.reference_id == reference.id)
    }
}

/// Template parameter names are not declaration identity. Canonicalize the
/// typed syntax by parameter position, preserving other qualified names/cv.
fn canonical_type(ty: &CppDeclaredType, parameters: &[String]) -> Option<CppDeclaredType> {
    let mut result = ty.clone();
    if let Some(i) = parameters.iter().position(|p| p == &ty.name) {
        if !ty.template_arguments.is_empty() {
            return None;
        }
        result.name = format!("${i}");
    } else if ty
        .name
        .split("::")
        .next()
        .is_some_and(|n| parameters.iter().any(|p| p == n))
    {
        return None; // Dependent nested names need substitution/name lookup.
    }
    result.template_arguments = ty
        .template_arguments
        .iter()
        .map(|t| canonical_type(t, parameters))
        .collect::<Option<_>>()?;
    Some(result)
}

fn identity(callable: &CppCallableDeclaration) -> Option<String> {
    let parameters = callable.template_parameters.as_ref()?;
    let types = callable
        .parameter_declared_types
        .iter()
        .map(|ty| canonical_type(ty.as_ref()?, parameters))
        .collect::<Option<Vec<_>>>()?;
    let return_type = canonical_type(callable.return_type.as_ref()?, parameters)?;
    let rvalue: Vec<_> = callable
        .parameter_types
        .iter()
        .map(|t| t.ends_with("&&"))
        .collect();
    serde_json::to_string(&(
        parameters.len(),
        types,
        rvalue,
        return_type,
        &callable.qualifiers,
    ))
    .ok()
}

pub(super) fn choose(
    reference: &ReferenceUse,
    visible: &[&SymbolDef],
    candidates: &[SymbolDef],
    arity: usize,
    types: &TypeIndex,
) -> Lookup<ResolvedTarget> {
    let call = types
        .template_call(reference)
        .ok_or(LookupFailure::Unspecified)?;
    let arguments = call.arguments.as_ref().ok_or(LookupFailure::Unspecified)?;
    let mut groups = BTreeMap::<(String, String), Vec<&SymbolDef>>::new();
    for symbol in visible {
        if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            return Err(LookupFailure::Unspecified);
        }
        let callable = types
            .written_callable(symbol)
            .ok_or(LookupFailure::Unspecified)?;
        let Some(parameters) = &callable.template_parameters else {
            continue; // Written template argument lists refer to templates.
        };
        if parameters.len() != arguments.len() {
            // Deduction/defaults and packs are deliberately not inferred here.
            return Err(LookupFailure::Unspecified);
        }
        if arity < callable.minimum_arity as usize || arity > callable.parameter_types.len() {
            continue;
        }
        if callable.is_virtual || callable.qualifiers.contains('&') {
            return Err(LookupFailure::Unspecified);
        }
        groups
            .entry((
                symbol.qualified_name.clone(),
                identity(callable).ok_or(LookupFailure::Unspecified)?,
            ))
            .or_default()
            .push(symbol);
    }
    if groups.len() != 1 {
        return Err(LookupFailure::Unspecified);
    }
    let ((qname, signature), declarations) = groups
        .into_iter()
        .next()
        .ok_or(LookupFailure::Unspecified)?;
    let internal = declarations.iter().any(|s| {
        types
            .written_callable(s)
            .is_some_and(|c| c.internal_linkage)
    });
    let visible_bodies: Vec<_> = declarations
        .iter()
        .copied()
        .filter(|s| s.range != s.name_range)
        .collect();
    let bodies: Vec<_> = if !visible_bodies.is_empty() {
        visible_bodies
    } else {
        candidates
            .iter()
            .filter(|s| {
                s.language == Language::Cpp
                    && s.qualified_name == qname
                    && s.range != s.name_range
                    && types.written_callable(s).is_some_and(|c| {
                        identity(c).as_deref() == Some(signature.as_str())
                            && ((!internal && !c.internal_linkage)
                                || s.file_id == reference.file_id)
                    })
                    && declarations.iter().any(|d| {
                        d.file_id == s.file_id || types.includes_declaration(s.file_id, d.file_id)
                    })
            })
            .collect()
    };
    let selected = match bodies.as_slice() {
        [body] => *body,
        [] => match declarations.as_slice() {
            [declaration] => *declaration,
            _ => return Err(LookupFailure::Unspecified),
        },
        _ => return Err(LookupFailure::Unspecified),
    };
    Ok(ResolvedTarget {
        symbol_id: selected.id,
        confidence: Confidence::certain(),
        strategy: ResolutionStrategy::ExactMatch,
        provenance: Provenance::TreeSitter,
    })
}

fn member_static(reference: &ReferenceUse, symbol: &SymbolDef, types: &TypeIndex) -> Lookup<bool> {
    let wanted = types
        .written_callable(symbol)
        .ok_or(LookupFailure::Unspecified)?;
    let visible = types.visible_files(reference.file_id);
    let mut matches = HashSet::new();
    for (id, static_) in types
        .member_callables
        .get(&symbol.qualified_name)
        .into_iter()
        .flatten()
    {
        let Some((file, position)) = types.callables.get(id) else {
            continue;
        };
        if !visible.contains(file) {
            continue;
        }
        let declaration = &types.files[file].callables[*position];
        let same = match (
            &wanted.template_parameters,
            &declaration.template_parameters,
        ) {
            (None, None) => {
                wanted.parameter_types == declaration.parameter_types
                    && wanted.qualifiers == declaration.qualifiers
            }
            (Some(_), Some(_)) => identity(wanted)
                .is_some_and(|key| identity(declaration).as_deref() == Some(key.as_str())),
            _ => false,
        };
        if same {
            matches.insert(*static_);
        }
    }
    if matches.len() != 1 {
        return Err(LookupFailure::Unspecified);
    }
    matches.into_iter().next().ok_or(LookupFailure::Unspecified)
}

pub(super) fn check_object(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    target: &SymbolDef,
    types: &TypeIndex,
) -> Lookup<()> {
    if !types.is_member_set(&target.qualified_name, &[target], reference.file_id) {
        return Ok(());
    }
    let target_static = member_static(reference, target, types)?;
    let qualified = reference
        .receiver
        .as_deref()
        .is_some_and(|receiver| reference.text == format!("{receiver}::{}", reference.name));
    if reference.receiver.is_some() && !qualified && reference.receiver.as_deref() != Some("this") {
        return Ok(()); // Ordinary receiver lookup already supplied the object.
    }
    if target_static && reference.receiver.as_deref() != Some("this") {
        return Ok(());
    }
    let caller = reference
        .source_symbol
        .and_then(|id| ctx.symbols_by_id.get(&id))
        .ok_or(LookupFailure::Unspecified)?;
    if member_static(reference, caller, types)? {
        return Err(LookupFailure::Unspecified);
    }
    let caller_owner = caller
        .qualified_name
        .rsplit_once("::")
        .map(|(scope, _)| scope);
    let target_owner = target
        .qualified_name
        .rsplit_once("::")
        .map(|(scope, _)| scope);
    if caller_owner != target_owner
        || types
            .files
            .get(&reference.file_id)
            .is_none_or(|file| file.this_capture_unavailable.contains(&reference.id))
    {
        return Err(LookupFailure::Unspecified);
    }
    let visible = types.visible_files(reference.file_id);
    let records: Vec<_> = types
        .records
        .get(caller_owner.ok_or(LookupFailure::Unspecified)?)
        .into_iter()
        .flatten()
        .filter(|(symbol, ty)| ty.is_definition && visible.contains(&symbol.file_id))
        .collect();
    let [(_, ty)] = records.as_slice() else {
        return Err(LookupFailure::Unspecified);
    };
    if !ty.lookup_supported {
        return Err(LookupFailure::Unspecified);
    }
    Ok(())
}
