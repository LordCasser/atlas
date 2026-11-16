//! Associate applicable compiler-selected direct calls with indexed invocations.
//!
//! The producer is responsible for compiler validity, configuration and source
//! consistency. This step checks indexed input identities and exact locations;
//! it is not a compiler or a validator of arbitrary third-party claims. It does
//! not mutate the store. Missing declarations can become declaration-only
//! endpoints in its result. A building pipeline may combine its results
//! with source resolutions before publishing; Ready queries must stay read-only.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path};

use db::Store;
use serde::{Deserialize, Serialize};
use types::{
    CallsiteId, Confidence, FileId, Language, Provenance, ReferenceId, ReferenceKind, ReferenceUse,
    ResolutionStrategy, ResolvedTarget, SymbolDef, SymbolId, SymbolKind, TextRange, layer,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompilerLocation {
    pub path: String,
    pub start_byte: u32,
    pub end_byte: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerDeclaration {
    pub name: String,
    /// Declaration category and semantic name supplied by the compiler producer.
    /// These do not describe an instantiated body or runtime receiver identity.
    pub kind: SymbolKind,
    pub qualified_name: String,
    pub location: CompilerLocation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerObservationKind {
    CallDeclaration,
    FieldReference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerDispatch {
    Direct,
    Virtual,
    NotACall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerObservation {
    pub kind: CompilerObservationKind,
    pub location: CompilerLocation,
    /// Complete written invocation, distinct from the callee's final name token.
    pub call_expression: Option<CompilerLocation>,
    pub declaration: CompilerDeclaration,
    pub definition: Option<CompilerDeclaration>,
    pub owner: Option<CompilerDeclaration>,
    pub dispatch: CompilerDispatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerBindingGapReason {
    DynamicDispatch,
    OwnerUnavailable,
    DefinitionUnavailable,
    DefinitionNotIndexed,
    CallExpressionUnavailable,
    InputIdentityUnavailable,
    IndexedInputMismatch,
    InvalidLocation,
    SymbolNotUnique,
    CallsiteNotUnique,
    CallerMismatch,
    ReferenceMismatch,
    ConflictingTargets,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerBindingGap {
    pub location: CompilerLocation,
    pub reason: CompilerBindingGapReason,
    /// Present only when the compiler observation matches an indexed invocation.
    pub reference_id: Option<ReferenceId>,
    /// A supplied indexed dependency whose identity prevented using the input.
    pub input_path: Option<String>,
    /// Source location supplied for a call's selected declaration on
    /// matching inputs. Navigation does not require an extracted symbol or a
    /// matching invocation; neither is manufactured from this position.
    pub declaration_location: Option<CompilerLocation>,
    /// An indexed declaration selected by this call observation, when
    /// its invocation and declaration locations could be associated precisely.
    /// This is a navigation endpoint, not a runtime implementation. Different
    /// observations may retain different declarations without a unique binding.
    pub declaration_id: Option<SymbolId>,
}

/// Explicit, applicable compiler observations for one building index. The caller
/// owns compiler configuration and dependency validity; `inputs` identifies the
/// indexed source bytes used by that computation, not arbitrary later files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerCallInput {
    pub inputs: BTreeMap<String, String>,
    pub observations: Vec<CompilerObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompilerCallResolution {
    pub reference: ReferenceUse,
    pub callsite_id: CallsiteId,
    pub target: ResolvedTarget,
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct CompilerCallBindings {
    pub resolved: Vec<CompilerCallResolution>,
    pub gaps: Vec<CompilerBindingGap>,
    /// New declaration-only endpoints, admitted only for retained direct bindings.
    pub declarations: Vec<SymbolDef>,
}

struct Association<'a> {
    store: &'a Store,
    inputs: &'a BTreeMap<String, String>,
    symbols: HashMap<FileId, Vec<SymbolDef>>,
    source_root: &'a Path,
    declarations: HashMap<SymbolId, SymbolDef>,
}

type Attempt<T> = anyhow::Result<Result<T, CompilerBindingGapReason>>;

impl Association<'_> {
    fn file(&self, at: &CompilerLocation) -> Attempt<FileId> {
        if at.start_byte >= at.end_byte {
            return Ok(Err(CompilerBindingGapReason::InvalidLocation));
        }
        self.indexed_file(&at.path)
    }

    fn indexed_file(&self, path: &str) -> Attempt<FileId> {
        if path.is_empty()
            || !Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return Ok(Err(CompilerBindingGapReason::InvalidLocation));
        }
        let Some(expected) = self.inputs.get(path).filter(|value| !value.is_empty()) else {
            return Ok(Err(CompilerBindingGapReason::InputIdentityUnavailable));
        };
        let id = FileId::generate(path);
        let Some(file) = self.store.get_file(&id)? else {
            return Ok(Err(CompilerBindingGapReason::IndexedInputMismatch));
        };
        if file.path != path || &file.content_hash != expected {
            return Ok(Err(CompilerBindingGapReason::IndexedInputMismatch));
        }
        Ok(Ok(id))
    }

    fn symbol(&mut self, declaration: &CompilerDeclaration) -> Attempt<SymbolId> {
        let file = match self.file(&declaration.location)? {
            Ok(file) => file,
            Err(reason) => return Ok(Err(reason)),
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = self.symbols.entry(file) {
            entry.insert(self.store.find_symbols_by_file(&file)?);
        }
        let matches: Vec<_> = self.symbols[&file]
            .iter()
            .filter(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
                ) && symbol.name == declaration.name
                    && symbol.name_range.start_byte == declaration.location.start_byte
                    && symbol.name_range.end_byte == declaration.location.end_byte
            })
            .collect();
        Ok(if matches.len() == 1 {
            Ok(matches[0].id)
        } else {
            Err(CompilerBindingGapReason::SymbolNotUnique)
        })
    }

    fn admit_declaration(&mut self, declaration: &CompilerDeclaration) -> Attempt<SymbolId> {
        use CompilerBindingGapReason as Gap;
        let file_id = match self.file(&declaration.location)? {
            Ok(file) => file,
            Err(reason) => return Ok(Err(reason)),
        };
        let file = self
            .store
            .get_file(&file_id)?
            .expect("checked indexed file");
        if !matches!(file.language, Language::Cpp | Language::C)
            || !matches!(
                declaration.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
            || declaration.name.is_empty()
            || !(declaration.qualified_name == declaration.name
                || declaration
                    .qualified_name
                    .ends_with(&format!("::{}", declaration.name)))
        {
            return Ok(Err(Gap::SymbolNotUnique));
        }
        // An existing, differently classified symbol is an extraction disagreement,
        // not permission to insert another identity at the same declaration.
        if self.symbols[&file_id].iter().any(|symbol| {
            symbol.name_range.start_byte == declaration.location.start_byte
                && symbol.name_range.end_byte == declaration.location.end_byte
        }) {
            return Ok(Err(Gap::SymbolNotUnique));
        }
        let Ok(source) = std::fs::read_to_string(self.source_root.join(&file.path)) else {
            return Ok(Err(Gap::IndexedInputMismatch));
        };
        if blake3::hash(source.as_bytes()).to_hex().as_str() != file.content_hash {
            return Ok(Err(Gap::IndexedInputMismatch));
        }
        let at = &declaration.location;
        let (start, end) = (at.start_byte as usize, at.end_byte as usize);
        if source.get(start..end) != Some(declaration.name.as_str()) {
            return Ok(Err(Gap::InvalidLocation));
        }
        let point = |offset: usize| {
            let before = &source.as_bytes()[..offset];
            (
                before.iter().filter(|byte| **byte == b'\n').count() as u32,
                before
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(offset, |newline| offset - newline - 1) as u32,
            )
        };
        let (start_line, start_column) = point(start);
        let (end_line, end_column) = point(end);
        let range = TextRange {
            start_byte: at.start_byte,
            end_byte: at.end_byte,
            start_line,
            start_column,
            end_line,
            end_column,
        };
        let id = SymbolId::generate(
            &file_id,
            file.language.as_str(),
            &declaration.qualified_name,
            declaration.kind.as_str(),
            Some(&format!("compiler-declaration:{}", at.start_byte)),
        );
        self.declarations.entry(id).or_insert_with(|| SymbolDef {
            id,
            kind: declaration.kind,
            name: declaration.name.clone(),
            qualified_name: declaration.qualified_name.clone(),
            symbol_path: Vec::new(),
            file_id,
            language: file.language,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: Vec::new(),
            layer: layer::COMPILER_DECLARATION.into(),
        });
        Ok(Ok(id))
    }

    fn reference(&self, observation: &CompilerObservation) -> Attempt<(CallsiteId, ReferenceUse)> {
        use CompilerBindingGapReason as Gap;
        let Some(expression) = &observation.call_expression else {
            return Ok(Err(Gap::CallExpressionUnavailable));
        };
        if expression.path != observation.location.path
            || expression.start_byte > observation.location.start_byte
            || expression.end_byte < observation.location.end_byte
        {
            return Ok(Err(Gap::InvalidLocation));
        }
        let file = match self.file(&observation.location)? {
            Ok(id) => id,
            Err(reason) => return Ok(Err(reason)),
        };
        let calls: Vec<_> = self
            .store
            .find_callsites_by_file(&file)?
            .into_iter()
            .filter(|call| {
                call.range.start_byte == expression.start_byte
                    && call.range.end_byte == expression.end_byte
                    && call.callee_range.as_ref().is_some_and(|range| {
                        range.start_byte <= observation.location.start_byte
                            && range.end_byte >= observation.location.end_byte
                    })
            })
            .collect();
        if calls.len() != 1 {
            return Ok(Err(Gap::CallsiteNotUnique));
        }
        let call = &calls[0];
        let Some(reference_id) = call.reference_id else {
            return Ok(Err(Gap::ReferenceMismatch));
        };
        let Some(reference) = self.store.get_reference_by_id(reference_id.as_bytes())? else {
            return Ok(Err(Gap::ReferenceMismatch));
        };
        if reference.kind != ReferenceKind::Call
            || reference.file_id != file
            || reference.source_symbol != Some(call.caller)
            || Some(reference.range) != call.callee_range
        {
            return Ok(Err(Gap::ReferenceMismatch));
        }
        Ok(Ok((call.id, reference)))
    }

    fn call(
        &mut self,
        observation: &CompilerObservation,
        callsite_id: CallsiteId,
        reference: ReferenceUse,
    ) -> Attempt<CompilerCallResolution> {
        use CompilerBindingGapReason as Gap;
        if observation.dispatch != CompilerDispatch::Direct {
            return Ok(Err(Gap::DynamicDispatch));
        }
        if !matches!(
            observation.declaration.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
        ) {
            return Ok(Err(Gap::ReferenceMismatch));
        }
        let Some(owner) = &observation.owner else {
            return Ok(Err(Gap::OwnerUnavailable));
        };
        // A direct call selects a declaration even when its definition is not
        // available in this translation unit. Preserve that endpoint without
        // guessing another definition from a matching name or signature.
        let target = observation
            .definition
            .as_ref()
            .unwrap_or(&observation.declaration);
        let owner_id = match self.symbol(owner)? {
            Ok(id) => id,
            Err(reason) => return Ok(Err(reason)),
        };
        if reference.source_symbol != Some(owner_id) {
            return Ok(Err(Gap::CallerMismatch));
        }
        let target_id = match self.symbol(target)? {
            Ok(id) => id,
            Err(CompilerBindingGapReason::SymbolNotUnique)
                if target.location == observation.declaration.location
                    && target.name == observation.declaration.name =>
            {
                if target.kind != observation.declaration.kind
                    || target.qualified_name != observation.declaration.qualified_name
                {
                    return Ok(Err(Gap::ReferenceMismatch));
                }
                match self.admit_declaration(&observation.declaration)? {
                    Ok(id) => id,
                    Err(reason) => return Ok(Err(reason)),
                }
            }
            Err(reason) => return Ok(Err(reason)),
        };
        if target.name != observation.declaration.name {
            return Ok(Err(Gap::ReferenceMismatch));
        }
        Ok(Ok(CompilerCallResolution {
            reference,
            callsite_id,
            target: ResolvedTarget {
                symbol_id: target_id,
                confidence: Confidence::new(1.0),
                strategy: ResolutionStrategy::Compiler,
                provenance: Provenance::Compiler,
            },
        }))
    }
}

/// Match compiler-selected direct calls against the same indexed source inputs.
/// `inputs` uses the index's existing per-file content identities, captured for
/// the compiler run. Callers must not populate it from an unrelated later index.
/// Missing definitions retain the selected declaration and a body gap. When its
/// source symbol was not extracted, `source_root` supplies the immutable bytes
/// needed to admit the producer's typed declaration at the exact name range.
/// This does not extract a body, container, signature or source visibility.
/// Fields, virtual dispatch, absent owners and conflicting targets cannot turn
/// into direct calls. No nearest-name or nearest-function fallback.
pub fn associate_compiler_calls(
    store: &Store,
    observations: &[CompilerObservation],
    inputs: &BTreeMap<String, String>,
    source_root: &Path,
    cancelled: &mut dyn FnMut() -> bool,
) -> anyhow::Result<CompilerCallBindings> {
    let mut association = Association {
        store,
        inputs,
        symbols: HashMap::new(),
        source_root,
        declarations: HashMap::new(),
    };
    let mut report = CompilerCallBindings::default();
    // A changed indexed dependency can invalidate calls in an unchanged file.
    // Validate the complete supplied indexed input set, not only edge endpoints.
    for path in inputs.keys() {
        anyhow::ensure!(!cancelled(), "compiler call association cancelled");
        if let Err(reason) = association.indexed_file(path)? {
            for observation in observations {
                anyhow::ensure!(!cancelled(), "compiler call association cancelled");
                if observation.kind == CompilerObservationKind::CallDeclaration {
                    report.gaps.push(CompilerBindingGap {
                        location: observation.location.clone(),
                        reason,
                        reference_id: None,
                        input_path: Some(path.clone()),
                        declaration_location: None,
                        declaration_id: None,
                    });
                }
            }
            return Ok(report);
        }
    }
    let mut selected: HashMap<ReferenceId, (CompilerCallResolution, CompilerLocation)> =
        HashMap::new();
    let mut conflicts = HashSet::new();
    let mut blocked_locations = HashSet::new();
    for observation in observations {
        anyhow::ensure!(!cancelled(), "compiler call association cancelled");
        if observation.kind != CompilerObservationKind::CallDeclaration {
            continue;
        }
        let location_key = observation.location.clone();
        let declaration_location = if observation.dispatch != CompilerDispatch::NotACall
            && association.file(&observation.declaration.location)?.is_ok()
        {
            Some(observation.declaration.location.clone())
        } else {
            None
        };
        let (reference_id, result, declaration_id) = match association.reference(observation)? {
            Ok((id, reference)) => {
                // The selected declaration can be useful even when the runtime
                // override, target symbol or compiler caller is unavailable.
                // Keep the observed declaration separate from any supplied
                // definition; this does not manufacture a callsite or binding.
                let declaration_id = if observation.dispatch != CompilerDispatch::NotACall {
                    association.symbol(&observation.declaration)?.ok()
                } else {
                    None
                };
                (
                    Some(reference.id),
                    association.call(observation, id, reference)?,
                    declaration_id,
                )
            }
            Err(reason) => (None, Err(reason), None),
        };
        match result {
            Err(reason) => {
                blocked_locations.insert(location_key);
                report.gaps.push(CompilerBindingGap {
                    location: observation.location.clone(),
                    reason,
                    reference_id,
                    input_path: None,
                    declaration_location,
                    declaration_id,
                });
            }
            Ok(binding) => {
                let id = binding.reference.id;
                let declaration_only = association
                    .declarations
                    .contains_key(&binding.target.symbol_id);
                if observation.definition.is_none() || declaration_only {
                    report.gaps.push(CompilerBindingGap {
                        location: observation.location.clone(),
                        reason: if observation.definition.is_none() {
                            CompilerBindingGapReason::DefinitionUnavailable
                        } else {
                            CompilerBindingGapReason::DefinitionNotIndexed
                        },
                        reference_id: Some(id),
                        input_path: None,
                        declaration_location: if declaration_only {
                            declaration_location.clone()
                        } else {
                            None
                        },
                        declaration_id: None,
                    });
                }
                if selected
                    .get(&id)
                    .is_some_and(|(old, _)| old.target.symbol_id != binding.target.symbol_id)
                {
                    conflicts.insert(id);
                    report.gaps.push(CompilerBindingGap {
                        location: observation.location.clone(),
                        reason: CompilerBindingGapReason::ConflictingTargets,
                        reference_id: Some(id),
                        input_path: None,
                        declaration_location: None,
                        declaration_id: None,
                    });
                }
                selected.insert(id, (binding, location_key));
            }
        }
    }
    report.resolved = selected
        .into_iter()
        .filter(|(id, (_, location))| {
            !conflicts.contains(id) && !blocked_locations.contains(location)
        })
        .map(|(_, (value, _))| value)
        .collect();
    report.resolved.sort_by_key(|binding| binding.reference.id);
    let retained: HashSet<_> = report
        .resolved
        .iter()
        .map(|binding| binding.target.symbol_id)
        .collect();
    report.declarations = association
        .declarations
        .into_values()
        .filter(|symbol| retained.contains(&symbol.id))
        .collect();
    report.declarations.sort_by_key(|symbol| symbol.id);
    Ok(report)
}

/// Replace primary source resolutions before GraphBuilder runs. A compiler
/// observation with an exact invocation identity also prevents weaker source
/// selection from hiding observed dynamic dispatch, conflicting targets or a
/// different caller. Missing compiler owners/definitions are producer limits;
/// they do not invalidate independently established source facts. Supplemental implicit
/// operator steps remain separate facts and do not resolve the written call.
/// This mutates a building Store only; existing edges must have been invalidated.
pub fn apply_compiler_call_bindings(
    store: &Store,
    source: &mut Vec<(ReferenceUse, ResolvedTarget)>,
    bindings: &CompilerCallBindings,
) -> anyhow::Result<()> {
    store.replace_symbols_in_layer(layer::COMPILER_DECLARATION, &bindings.declarations)?;
    // A producer's missing definition is not evidence against a body already
    // selected from source. Retain that result only when its declaration identity
    // agrees with the compiler selection under the existing C++ association rules.
    // Keep source provenance: the compiler did not establish this body.
    let selected: HashMap<_, _> = bindings
        .resolved
        .iter()
        .map(|binding| (binding.reference.id, binding))
        .collect();
    let mut types = None;
    let mut preserved = HashMap::new();
    for (reference, target) in source.iter() {
        let Some(binding) = selected.get(&reference.id) else {
            continue;
        };
        if target.strategy != ResolutionStrategy::ExactMatch
            || target.provenance != Provenance::TreeSitter
            || target.symbol_id == binding.target.symbol_id
        {
            continue;
        }
        let (Some(declaration), Some(body)) = (
            store.find_symbol_by_id(&binding.target.symbol_id)?,
            store.find_symbol_by_id(&target.symbol_id)?,
        ) else {
            continue;
        };
        if declaration.range != declaration.name_range || body.range == body.name_range {
            continue;
        }
        if types.is_none() {
            types = Some(crate::cpp::TypeIndex::build(store)?);
        }
        if types
            .as_ref()
            .is_some_and(|types| types.matches_selected_declaration(&declaration, &body))
        {
            preserved.insert(reference.id, target.clone());
        }
    }
    // Call selection and declaration/body association are separate facts. A
    // source resolver may lack the receiver or argument facts needed to select
    // this call, while still associating the compiler-selected declaration with
    // one body. Reuse the same typed rule as declaration navigation; names alone
    // never suffice, and multiple matching bodies retain the declaration.
    let mut bodies: HashMap<SymbolId, Option<SymbolId>> = HashMap::new();
    for binding in &bindings.resolved {
        if preserved.contains_key(&binding.reference.id) {
            continue;
        }
        let Some(declaration) = store.find_symbol_by_id(&binding.target.symbol_id)? else {
            continue;
        };
        if declaration.language != Language::Cpp || declaration.range != declaration.name_range {
            continue;
        }
        let body = if let Some(found) = bodies.get(&declaration.id) {
            *found
        } else {
            if types.is_none() {
                types = Some(crate::cpp::TypeIndex::build(store)?);
            }
            let mut definitions = store
                .find_symbols_by_qname(&declaration.qualified_name)?
                .into_iter()
                .filter(|body| {
                    types
                        .as_ref()
                        .unwrap()
                        .matches_selected_declaration(&declaration, body)
                });
            let first = definitions.next();
            let unique = if definitions.next().is_none() {
                first.map(|body| body.id)
            } else {
                None
            };
            bodies.insert(declaration.id, unique);
            unique
        };
        if let Some(body) = body {
            // Compiler provenance describes call selection. Its missing-body
            // observation remains a producer limit; the body association above
            // comes from supported source declarations, not the compiler AST.
            let mut target = binding.target.clone();
            target.symbol_id = body;
            preserved.insert(binding.reference.id, target);
        }
    }
    let affected: HashSet<_> = bindings
        .resolved
        .iter()
        .map(|binding| binding.reference.id)
        .chain(
            bindings
                .gaps
                .iter()
                .filter(|gap| {
                    matches!(
                        gap.reason,
                        CompilerBindingGapReason::DynamicDispatch
                            | CompilerBindingGapReason::ConflictingTargets
                            | CompilerBindingGapReason::CallerMismatch
                    )
                })
                .filter_map(|gap| gap.reference_id),
        )
        .collect();
    source.retain(|(reference, target)| {
        !affected.contains(&reference.id) || target.strategy == ResolutionStrategy::ImplicitOperator
    });
    store.invalidate_references(&affected.into_iter().collect::<Vec<_>>())?;
    let updates: Vec<_> = bindings
        .resolved
        .iter()
        .map(|binding| {
            (
                binding.reference.id,
                preserved
                    .get(&binding.reference.id)
                    .unwrap_or(&binding.target)
                    .clone(),
            )
        })
        .collect();
    store.batch_update_resolutions(&updates)?;
    source.extend(bindings.resolved.iter().map(|binding| {
        (
            binding.reference.clone(),
            preserved
                .get(&binding.reference.id)
                .unwrap_or(&binding.target)
                .clone(),
        )
    }));
    Ok(())
}
