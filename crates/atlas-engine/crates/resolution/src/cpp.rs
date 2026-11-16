//! C++ call lookup from visible declarations and a limited receiver-type subset.
//!
//! General expression typing, using directives, template instantiation and overload conversions
//! remain unsupported. Calls must never fall through to project-wide
//! simple-name/proximity selection.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{context::ResolutionContext, cpp_captures::CaptureEffect};
use types::cpp::{
    CppCallableDeclaration, CppDeclaredType, CppFileTypes, CppRecordType, CppValueType,
};
use types::*;

mod arguments;
mod inheritance;
mod lambdas;
pub mod navigation;
mod operators;
mod templates;

/// Match written declarations using the same bounded identity rule as source
/// body association. Visibility and call applicability are separate prerequisites.
pub fn declarations_match(
    declaration: &SymbolDef,
    selected: &CppCallableDeclaration,
    body: &SymbolDef,
    defined: &CppCallableDeclaration,
    body_includes_declaration: bool,
) -> bool {
    declaration.language == Language::Cpp
        && body.language == Language::Cpp
        && declaration.qualified_name == body.qualified_name
        && selected.parameter_types == defined.parameter_types
        && selected.qualifiers == defined.qualifiers
        && (!(selected.internal_linkage || defined.internal_linkage)
            || declaration.file_id == body.file_id)
        && (declaration.file_id == body.file_id
            || parameter_names_are_fundamental(&selected.parameter_types)
            || body_includes_declaration)
}

fn parameter_names_are_fundamental(parameters: &[String]) -> bool {
    parameters.iter().all(|ty| {
        ty.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .filter(|token| !token.is_empty())
            .all(|token| {
                matches!(
                    token,
                    "const"
                        | "volatile"
                        | "void"
                        | "bool"
                        | "char"
                        | "signed"
                        | "unsigned"
                        | "short"
                        | "int"
                        | "long"
                        | "float"
                        | "double"
                        | "wchar_t"
                        | "char8_t"
                        | "char16_t"
                        | "char32_t"
                )
            })
    })
}

/// Most remaining failures have no precise diagnostic yet. Keep those unknown
/// rather than labeling every unsupported operation as a missing definition.
#[derive(Debug)]
pub(crate) enum LookupFailure {
    Unspecified,
    Type(Box<types::cpp::CppTypeLookupFailure>),
}

impl LookupFailure {
    fn virtual_member(reference: &ReferenceUse, record: &SymbolDef, member: &SymbolDef) -> Self {
        Self::Type(Box::new(types::cpp::CppTypeLookupFailure {
            kind: types::cpp::CppTypeLookupFailureKind::VirtualMemberUnresolved,
            name: reference.name.clone(),
            scope: record.qualified_name.clone(),
            file_id: reference.file_id,
            range: reference.range,
            related_declarations: vec![(member.file_id, member.range)],
        }))
    }

    fn name_restricted(
        name: &str,
        scope: &str,
        reference: &ReferenceUse,
        declarations: Vec<(FileId, TextRange)>,
    ) -> Self {
        Self::Type(Box::new(types::cpp::CppTypeLookupFailure {
            kind: types::cpp::CppTypeLookupFailureKind::NameLookupRestricted,
            name: name.to_string(),
            scope: scope.to_string(),
            file_id: reference.file_id,
            range: reference.range,
            related_declarations: Vec::new(),
        }))
        .with_declarations(declarations)
    }

    fn with_declarations(mut self, mut declarations: Vec<(FileId, TextRange)>) -> Self {
        declarations.sort_by_key(|(file, range)| (*file, range.start_byte, range.end_byte));
        declarations.dedup();
        if let Self::Type(failure) = &mut self {
            failure.related_declarations = declarations;
        }
        self
    }
}

type Lookup<T> = Result<T, LookupFailure>;

// Shared by recursive lexical/member type searches. Source spelling and lookup
// point identify an active search, not a persistent evidence identity.
type TypeLookupPath = HashSet<(String, String, FileId, u32, bool)>;

/// Names bind where they were written. A member use can require the complete
/// definition of that identity in a later context, without rebinding the name.
#[derive(Clone, Copy)]
struct TypeLookup<'a> {
    site: &'a ReferenceUse,
    imported: &'a HashSet<FileId>,
    complete_at: Option<&'a ReferenceUse>,
}

impl<'a> TypeLookup<'a> {
    fn at(site: &'a ReferenceUse, imported: &'a HashSet<FileId>) -> Self {
        Self {
            site,
            imported,
            complete_at: None,
        }
    }
}

/// A named type's underlying identity, independent of member inventory and
/// of the pointer/reference/cv layers at its use. Alias declarations do not
/// create another class or an associated namespace.
enum NamedType<'a> {
    Fundamental(String),
    Record(&'a SymbolDef, &'a CppRecordType),
}

impl<'a> NamedType<'a> {
    fn record(self) -> Lookup<(&'a SymbolDef, &'a CppRecordType)> {
        match self {
            Self::Record(symbol, ty) => Ok((symbol, ty)),
            Self::Fundamental(_) => Err(LookupFailure::Unspecified),
        }
    }

    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fundamental(a), Self::Fundamental(b)) => a == b,
            (Self::Record(a, _), Self::Record(b, _)) => a.id == b.id,
            _ => false,
        }
    }
}

struct ValueType {
    declared: CppDeclaredType,
    scope: String,
    declaration: Option<(FileId, TextRange)>,
}

struct LocalValue<'a> {
    value: &'a CppValueType,
    capture: CaptureEffect,
}

/// Only declaration facts are indexed here; no source reads during resolution.
#[derive(Debug, Default)]
pub(crate) struct TypeIndex {
    files: HashMap<FileId, CppFileTypes>,
    arguments: HashMap<ReferenceId, (FileId, usize)>,
    callables: HashMap<SymbolId, (FileId, usize)>,
    /// Direct class declarations supply member/static identity for a separately
    /// written definition. A qualified definition alone cannot supply it.
    member_callables: HashMap<String, Vec<(SymbolId, bool)>>,
    includes: HashMap<FileId, Vec<(FileId, u32)>>,
    records: HashMap<String, Vec<(SymbolDef, CppRecordType)>>,
    aliases: HashMap<String, Vec<(SymbolDef, types::cpp::CppTypeAlias)>>,
    fields: HashMap<String, Vec<(SymbolDef, CppValueType)>>,
    names: HashMap<String, Vec<(FileId, u32, SymbolKind)>>,
    lookup_limits: HashMap<String, HashSet<FileId>>,
    lookup_name_limits: HashMap<String, HashSet<FileId>>,
    initializers: HashMap<ReferenceId, ReferenceUse>,
    factory_candidates: HashMap<String, Vec<SymbolDef>>,
    operators: Vec<SymbolDef>,
    specialized_templates: HashMap<String, HashSet<FileId>>,
}

impl TypeIndex {
    pub(crate) fn build(store: &db::Store) -> anyhow::Result<Self> {
        let files = store.all_cpp_types()?;
        let mut symbols = Vec::new();
        for file in files.keys() {
            symbols.extend(store.find_symbols_by_file(file)?);
        }
        Self::from_symbols(&symbols, files, store)
    }

    pub(crate) fn from_symbols(
        symbols: &[SymbolDef],
        files: HashMap<FileId, CppFileTypes>,
        store: &db::Store,
    ) -> anyhow::Result<Self> {
        let source_symbols;
        let symbols = if symbols
            .iter()
            .any(|symbol| symbol.layer == layer::COMPILER_DECLARATION)
        {
            source_symbols = symbols
                .iter()
                .filter(|symbol| symbol.layer != layer::COMPILER_DECLARATION)
                .cloned()
                .collect::<Vec<_>>();
            source_symbols.as_slice()
        } else {
            symbols
        };
        let by_id: HashMap<_, _> = symbols.iter().map(|s| (s.id, s)).collect();
        let mut index = Self {
            files,
            ..Default::default()
        };
        let mut factory_names = HashSet::new();
        for (file, facts) in &index.files {
            let ids: HashSet<_> = facts
                .values
                .iter()
                .filter_map(|v| v.initializer_call)
                .collect();
            if ids.is_empty() {
                continue;
            }
            for reference in store.find_references_by_file(file)? {
                if ids.contains(&reference.id) {
                    factory_names.insert(reference.name.clone());
                    index.initializers.insert(reference.id, reference);
                }
            }
        }
        // Retain candidates only for actual auto initializer names, rather than
        // duplicating the complete project symbol index for type deduction.
        for symbol in symbols
            .iter()
            .filter(|s| s.language == Language::Cpp && factory_names.contains(&s.name))
        {
            index
                .factory_candidates
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.clone());
        }
        let include_paths: Vec<String> = store
            .get_metadata(crate::KEY_INCLUDE_PATHS)?
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or_default();
        for file in index.files.keys() {
            let mut includes = Vec::new();
            for import in store.find_imports_by_file(file)? {
                if import.kind == ImportKind::Include
                    && let Some(target) = store.resolve_include_file(&import, &include_paths)?
                {
                    includes.push((target.file_id, import.range.start_byte));
                }
            }
            includes.sort_unstable();
            includes.dedup();
            index.includes.insert(*file, includes);
        }
        for symbol in symbols
            .iter()
            .filter(|symbol| symbol.language == Language::Cpp)
        {
            if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                && symbol
                    .container
                    .and_then(|id| by_id.get(&id))
                    .is_some_and(|owner| {
                        matches!(owner.kind, SymbolKind::Class | SymbolKind::Struct)
                    })
            {
                index
                    .member_callables
                    .entry(symbol.qualified_name.clone())
                    .or_default()
                    .push((symbol.id, symbol.static_));
            }
            if symbol.name == "operator->" {
                index.operators.push(symbol.clone());
            }
            index
                .names
                .entry(symbol.qualified_name.clone())
                .or_default()
                .push((symbol.file_id, symbol.name_range.start_byte, symbol.kind));
        }
        for (file, facts) in &index.files {
            for (position, (reference, _)) in facts.arguments.iter().enumerate() {
                index.arguments.insert(*reference, (*file, position));
            }
            for name in &facts.specialized_templates {
                index
                    .specialized_templates
                    .entry(name.clone())
                    .or_default()
                    .insert(*file);
            }
            for (position, callable) in facts.callables.iter().enumerate() {
                index
                    .callables
                    .insert(callable.symbol_id, (*file, position));
            }
            for limit in &facts.lookup_limits {
                // A local import is evaluated at the lookup point, never
                // promoted to the enclosing class/namespace or its includers.
                if limit.block_range.is_some() {
                    continue;
                }
                if let Some(name) = &limit.name {
                    let name = if limit.scope.is_empty() {
                        name.clone()
                    } else {
                        format!("{}::{name}", limit.scope)
                    };
                    index
                        .lookup_name_limits
                        .entry(name)
                        .or_default()
                        .insert(*file);
                } else {
                    index
                        .lookup_limits
                        .entry(limit.scope.clone())
                        .or_default()
                        .insert(*file);
                }
            }
            for alias in &facts.aliases {
                if let Some(symbol) = by_id.get(&alias.symbol_id) {
                    index
                        .aliases
                        .entry(symbol.qualified_name.clone())
                        .or_default()
                        .push(((*symbol).clone(), alias.clone()));
                }
            }
            for record in &facts.records {
                if let Some(symbol) = by_id.get(&record.symbol_id) {
                    index
                        .records
                        .entry(symbol.qualified_name.clone())
                        .or_default()
                        .push(((*symbol).clone(), record.clone()));
                }
            }
            for value in &facts.values {
                if let Some(symbol) = value.symbol_id.and_then(|id| by_id.get(&id)) {
                    index
                        .fields
                        .entry(symbol.qualified_name.clone())
                        .or_default()
                        .push(((*symbol).clone(), value.clone()));
                }
            }
        }
        Ok(index)
    }

    fn callable(&self, symbol: &SymbolDef) -> Option<&CppCallableDeclaration> {
        self.written_callable(symbol)
            .filter(|callable| callable.template_parameters.is_none())
    }

    fn written_callable(&self, symbol: &SymbolDef) -> Option<&CppCallableDeclaration> {
        let (file, position) = self.callables.get(&symbol.id)?;
        self.files.get(file)?.callables.get(*position)
    }

    /// Check a source body against a selected declaration using the existing
    /// identity, parameter, qualifier and visibility rules. This does not select
    /// a call overload or prove that the matching body is unique.
    pub(crate) fn matches_selected_declaration(
        &self,
        declaration: &SymbolDef,
        body: &SymbolDef,
    ) -> bool {
        if declaration.language != Language::Cpp
            || body.language != Language::Cpp
            || declaration.qualified_name != body.qualified_name
            || declaration.range != declaration.name_range
            || body.range == body.name_range
        {
            return false;
        }
        let (Some(selected), Some(defined)) = (self.callable(declaration), self.callable(body))
        else {
            return false;
        };
        declarations_match(
            declaration,
            selected,
            body,
            defined,
            declaration.file_id != body.file_id
                && !parameter_names_are_fundamental(&selected.parameter_types)
                && self.includes_declaration(body.file_id, declaration.file_id),
        )
    }

    fn can_associate(
        &self,
        declaration: &SymbolDef,
        body: &SymbolDef,
        parameters: &[String],
    ) -> bool {
        declaration.file_id == body.file_id
            // Fundamental types have no file-local name lookup. A named type
            // needs shared declaration visibility until type/alias equivalence
            // across separate translation units is available.
            || parameter_names_are_fundamental(parameters)
            || self.includes_declaration(body.file_id, declaration.file_id)
    }

    fn includes_declaration(&self, body: FileId, declaration: FileId) -> bool {
        // Follow only resolved include paths in the selected input. This
        // provides declaration visibility, not a preprocessed build variant.
        // Walk on demand instead of retaining a quadratic transitive closure.
        let mut pending = vec![body];
        let mut examined = HashSet::new();
        while let Some(file) = pending.pop() {
            if !examined.insert(file) {
                continue;
            }
            if let Some(includes) = self.includes.get(&file) {
                if includes.iter().any(|(target, _)| *target == declaration) {
                    return true;
                }
                pending.extend(includes.iter().map(|(target, _)| *target));
            }
        }
        false
    }

    fn receiver<'a>(
        &'a self,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        caller_owner: &str,
        implicit: &mut Vec<ResolvedTarget>,
        deductions_remaining: &mut usize,
    ) -> Lookup<(&'a SymbolDef, &'a CppRecordType)> {
        let imported = self.visible_files(reference.file_id);
        let receiver = reference
            .receiver
            .as_deref()
            .ok_or(LookupFailure::Unspecified)?;
        let (name, explicit_field) = receiver
            .strip_prefix("this->")
            .map_or((receiver, false), |name| (name, true));
        if !plain_identifier(name) {
            return Err(LookupFailure::Unspecified);
        }
        if !explicit_field && let Some(value) = self.local_value(name, reference, ctx)? {
            return self.value_record(
                value,
                reference,
                ctx,
                reference.file_id,
                implicit,
                deductions_remaining,
            );
        }
        self.captured_field_object(reference, ctx, (!explicit_field).then_some(name))?;
        let fields = self
            .fields
            .get(&format!("{caller_owner}::{name}"))
            .ok_or(LookupFailure::Unspecified)?;
        let visible: Vec<_> = fields
            .iter()
            .filter(|(s, _)| s.file_id == reference.file_id || imported.contains(&s.file_id))
            .collect();
        let [(symbol, value)] = visible.as_slice() else {
            return Err(LookupFailure::Unspecified);
        };
        self.value_record(
            LocalValue {
                value,
                capture: CaptureEffect::default(),
            },
            reference,
            ctx,
            symbol.file_id,
            implicit,
            deductions_remaining,
        )
    }

    fn type_name_shadowed(value: &CppValueType, scope: ScopeId, ctx: &ResolutionContext) -> bool {
        let Some(ty) = &value.declared_type else {
            return false;
        };
        if ty.name.starts_with("::") {
            return false;
        }
        let name = ty.name.split("::").next().unwrap_or("");
        let mut scope = Some(scope);
        while let Some(id) = scope {
            if ctx.bindings.iter().any(|binding| {
                binding.scope_id == id
                    && binding.name == name
                    && binding.visible_from_byte <= value.declaration_range.start_byte
            }) {
                return true;
            }
            scope = ctx.scope_parents.get(&id).copied();
        }
        false
    }

    fn value_record<'a>(
        &'a self,
        value: LocalValue<'_>,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        declaration_file: FileId,
        implicit: &mut Vec<ResolvedTarget>,
        deductions_remaining: &mut usize,
    ) -> Lookup<(&'a SymbolDef, &'a CppRecordType)> {
        let ValueType {
            declared: mut ty,
            scope: lookup_scope,
            declaration: origin,
        } = self.value_type(value.value, ctx, deductions_remaining)?;
        value.capture.apply(&mut ty);
        if ty.volatile {
            return Err(LookupFailure::Unspecified);
        } // cv/ref overload selection is not modeled yet.
        let arrow = reference
            .text
            .rsplit_once(&reference.name)
            .ok_or(LookupFailure::Unspecified)?
            .0
            .trim_end()
            .ends_with("->");
        let mut site = reference.clone();
        let (file, range) = origin.unwrap_or((declaration_file, value.value.declaration_range));
        site.file_id = file;
        site.range = range;
        let imported = self.visible_files(file);
        let lookup = TypeLookup {
            site: &site,
            imported: &imported,
            complete_at: Some(reference),
        };
        if arrow && !ty.pointer {
            return self.arrow_record(&ty, &lookup_scope, lookup, implicit);
        }
        if arrow != ty.pointer || ty.const_ || !ty.template_arguments.is_empty() {
            return Err(LookupFailure::Unspecified);
        }
        self.lookup_record(&ty, &lookup_scope, lookup)
    }

    fn value_type(
        &self,
        value: &CppValueType,
        ctx: &ResolutionContext,
        deductions_remaining: &mut usize,
    ) -> Lookup<ValueType> {
        if let Some(ty) = &value.declared_type {
            return Ok(ValueType {
                declared: ty.clone(),
                scope: value.lookup_scope.clone(),
                declaration: None,
            });
        }
        // One per-call budget is shared across argument and receiver deduction,
        // bounding both cycles and branching chains of auto initializers.
        *deductions_remaining = deductions_remaining
            .checked_sub(1)
            .ok_or(LookupFailure::Unspecified)?;
        let call = self
            .initializers
            .get(&value.initializer_call.ok_or(LookupFailure::Unspecified)?)
            .ok_or(LookupFailure::Unspecified)?;
        // Reuse the same receiver and argument checks as an ordinary call.
        // Unsupported expressions remain unknown there; recursive auto
        // receivers share the deduction budget consumed above.
        let candidates = self
            .factory_candidates
            .get(&call.name)
            .ok_or(LookupFailure::Unspecified)?;
        let imported = self.visible_files(call.file_id);
        let target = resolve_call_with_budget(
            call,
            ctx,
            candidates,
            &imported,
            Some(self),
            deductions_remaining,
        )?;
        let symbol = candidates
            .iter()
            .find(|s| s.id == target.symbol_id)
            .ok_or(LookupFailure::Unspecified)?;
        let selected = self.callable(symbol).ok_or(LookupFailure::Unspecified)?;
        // Deduction uses the declaration visible at the initializer, not an
        // implementation's possibly differently-spelled return type elsewhere.
        let returns: Vec<_> = candidates
            .iter()
            .filter(|s| {
                s.qualified_name == symbol.qualified_name
                    && (s.file_id == call.file_id || imported.contains(&s.file_id))
                    && (s.file_id != call.file_id
                        || s.name_range.start_byte <= call.range.start_byte
                        || s.kind == SymbolKind::Method)
                    && self.callable(s).is_some_and(|c| {
                        c.parameter_types == selected.parameter_types
                            && c.qualifiers == selected.qualifiers
                    })
            })
            .map(|s| (s, self.callable(s).and_then(|c| c.return_type.as_ref())))
            .collect();
        // A function body selected for navigation does not replace the visible
        // prototype's type binding. Prefer that declaration independently of
        // Store/symbol iteration order and of later imports in the body file.
        let (declaration, written) = returns
            .iter()
            .min_by_key(|(symbol, _)| {
                (
                    symbol.range != symbol.name_range,
                    symbol.file_id,
                    symbol.name_range.start_byte,
                )
            })
            .ok_or(LookupFailure::Unspecified)?;
        let mut ty = (*written).ok_or(LookupFailure::Unspecified)?.clone();
        if returns.iter().any(|(_, other)| *other != Some(&ty)) {
            return Err(LookupFailure::Unspecified);
        }
        // Plain auto copies the expression's type, dropping reference and
        // top-level cv. Pointee cv remains relevant to member selection.
        ty.reference = false;
        if !ty.pointer {
            ty.const_ = false;
            ty.volatile = false;
        }
        let scope = declaration
            .qualified_name
            .rsplit_once("::")
            .map_or("", |(scope, _)| scope);
        Ok(ValueType {
            declared: ty,
            scope: scope.into(),
            declaration: Some((declaration.file_id, declaration.name_range)),
        })
    }

    fn lookup_record<'a>(
        &'a self,
        ty: &CppDeclaredType,
        owner: &str,
        lookup: TypeLookup<'_>,
    ) -> Lookup<(&'a SymbolDef, &'a CppRecordType)> {
        self.lookup_type_in(ty, owner, lookup, &mut TypeLookupPath::new(), false)?
            .ok_or(LookupFailure::Unspecified)?
            .record()
    }

    fn lookup_type_in<'a>(
        &'a self,
        ty: &CppDeclaredType,
        owner: &str,
        lookup: TypeLookup<'_>,
        path: &mut TypeLookupPath,
        members_only: bool,
    ) -> Lookup<Option<NamedType<'a>>> {
        let TypeLookup {
            site: reference,
            imported,
            complete_at,
        } = lookup;
        if ty.template_arguments.is_empty()
            && let Some(name) = arguments::fundamental(&ty.name)
        {
            return Ok(Some(NamedType::Fundamental(name)));
        }
        let key = (
            ty.name.clone(),
            owner.to_string(),
            reference.file_id,
            reference.range.start_byte,
            members_only,
        );
        if path.len() >= 64 || !path.insert(key.clone()) {
            return Err(LookupFailure::Unspecified);
        }
        let result = (|| {
            use types::cpp::{CppTypeLookupFailure, CppTypeLookupFailureKind as Kind};
            let failure = |kind| {
                LookupFailure::Type(Box::new(CppTypeLookupFailure {
                    kind,
                    name: ty.name.clone(),
                    scope: owner.to_string(),
                    file_id: reference.file_id,
                    range: reference.range,
                    related_declarations: Vec::new(),
                }))
            };
            if self.block_lookup_limited(&ty.name, reference) {
                let declarations = self.files[&reference.file_id]
                    .lookup_limits
                    .iter()
                    .filter(|limit| limit.limits_local_lookup(&ty.name, reference.range.start_byte))
                    .map(|limit| (reference.file_id, limit.declaration_range))
                    .collect();
                return Err(failure(Kind::LookupRestricted).with_declarations(declarations));
            }
            let mut scope = if ty.name.starts_with("::") { "" } else { owner };
            loop {
                if self.lookup_limits.get(scope).is_some_and(|files| {
                    files
                        .iter()
                        .any(|file| *file == reference.file_id || imported.contains(file))
                }) {
                    return Err(failure(Kind::LookupRestricted).with_declarations(
                        self.lookup_limit_declarations(scope, None, reference.file_id, imported),
                    ));
                }
                let name = if !scope.is_empty()
                    && self.records.contains_key(scope)
                    && scope.rsplit("::").next() == Some(ty.name.as_str())
                {
                    // The injected class name denotes the class, not its constructor.
                    // For inherited lookup, retain any same-name non-type or macro
                    // restriction instead of treating the constructor spelling as
                    // evidence that all declarations with this name are types.
                    if members_only {
                        let member = format!("{scope}::{}", ty.name);
                        if self.imported_name_limited(&member, reference.file_id, imported) {
                            return Err(failure(Kind::LookupRestricted).with_declarations(
                                self.imported_name_declarations(
                                    &member,
                                    reference.file_id,
                                    imported,
                                ),
                            ));
                        }
                        if self.names.get(&member).into_iter().flatten().any(
                            |(file, start, kind)| {
                                (*file == reference.file_id || imported.contains(file))
                                    && (*file != reference.file_id
                                        || *start < reference.range.start_byte)
                                    && !matches!(
                                        kind,
                                        SymbolKind::Function
                                            | SymbolKind::Method
                                            | SymbolKind::Constructor
                                    )
                            },
                        ) {
                            return Err(failure(Kind::NameHidingUnsupported));
                        }
                    }
                    scope.to_string()
                } else if scope.is_empty() {
                    ty.name.trim_start_matches("::").to_string()
                } else {
                    format!("{scope}::{}", ty.name)
                };
                if self.imported_name_limited(&name, reference.file_id, imported) {
                    return Err(failure(Kind::LookupRestricted).with_declarations(
                        self.imported_name_declarations(&name, reference.file_id, imported),
                    ));
                }
                // An alias in a qualifier (Alias::Device) must not be skipped in
                // favor of an outer same-spelled namespace or class.
                let mut prefix = name.rsplit_once("::").map(|(prefix, _)| prefix);
                while let Some(qualifier) = prefix {
                    if self
                        .names
                        .get(qualifier)
                        .into_iter()
                        .flatten()
                        .any(|(file, start, kind)| {
                            (*file == reference.file_id || imported.contains(file))
                                && (*file != reference.file_id
                                    || *start < reference.range.start_byte)
                                && *kind == SymbolKind::TypeAlias
                        })
                    {
                        return Err(failure(Kind::NameHidingUnsupported));
                    }
                    prefix = qualifier.rsplit_once("::").map(|(prefix, _)| prefix);
                }
                // Bind the nearest alias before looking outside this scope.
                // Only a complete, visible ordinary declaration can supply its
                // target; never canonicalize an unknown typedef by spelling.
                let aliases: Vec<_> = self
                    .aliases
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .filter(|(symbol, _)| {
                        (symbol.file_id == reference.file_id || imported.contains(&symbol.file_id))
                            && (symbol.file_id != reference.file_id
                                || symbol.name_range.start_byte < reference.range.start_byte)
                    })
                    .collect();
                if !aliases.is_empty() {
                    let [(symbol, alias)] = aliases.as_slice() else {
                        return Err(failure(Kind::DefinitionAmbiguous));
                    };
                    if !ty.template_arguments.is_empty() {
                        return Err(failure(Kind::TemplateUnsupported));
                    }
                    if self
                        .names
                        .get(&name)
                        .into_iter()
                        .flatten()
                        .any(|(file, start, kind)| {
                            (*file == reference.file_id || imported.contains(file))
                                && (*file != reference.file_id
                                    || *start < reference.range.start_byte)
                                && *kind != SymbolKind::TypeAlias
                        })
                    {
                        return Err(failure(Kind::NameHidingUnsupported));
                    }
                    let target = alias
                        .target
                        .as_ref()
                        .ok_or_else(|| failure(Kind::NameHidingUnsupported))?;
                    let site = ReferenceUse {
                        file_id: symbol.file_id,
                        range: alias.target_range,
                        ..reference.clone()
                    };
                    let scope = symbol
                        .qualified_name
                        .rsplit_once("::")
                        .map_or("", |(scope, _)| scope);
                    return self.lookup_type_in(
                        target,
                        scope,
                        TypeLookup {
                            site: &site,
                            imported: &self.visible_files(symbol.file_id),
                            complete_at,
                        },
                        path,
                        false,
                    );
                }
                // Enums, fields and functions can hide an outer class name. A
                // record-only index must not silently step over those declarations.
                if self
                    .names
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .any(|(file, start, kind)| {
                        (*file == reference.file_id || imported.contains(file))
                            && (*file != reference.file_id || *start < reference.range.start_byte)
                            && !matches!(kind, SymbolKind::Class | SymbolKind::Struct)
                    })
                {
                    return Err(failure(Kind::NameHidingUnsupported));
                }
                let visible: Vec<_> = self
                    .records
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .filter(|(symbol, _)| {
                        (symbol.file_id == reference.file_id || imported.contains(&symbol.file_id))
                            && (symbol.file_id != reference.file_id
                                || symbol.name_range.start_byte < reference.range.start_byte)
                    })
                    .collect();
                if !visible.is_empty() {
                    // An incomplete local declaration still hides an outer type;
                    // a declaration plus its definition is not an ambiguity.
                    let mut definitions: Vec<_> = visible
                        .iter()
                        .copied()
                        .filter(|(_, record)| record.is_definition)
                        .collect();
                    if definitions.is_empty()
                        && let Some(use_site) = complete_at
                    {
                        // The nearest visible declaration has already bound
                        // `name`. Only complete this identity; never retry name
                        // lookup with the member site's imports or namespace.
                        if !ty.template_arguments.is_empty()
                            || visible
                                .iter()
                                .any(|(_, record)| record.template_parameters.is_some())
                        {
                            return Err(failure(Kind::TemplateUnsupported));
                        }
                        if visible.iter().any(|(_, record)| !record.identity_supported) {
                            return Err(failure(Kind::LookupRestricted));
                        }
                        let use_files = self.visible_files_at(use_site);
                        definitions = self.records[&name]
                            .iter()
                            .filter(|(symbol, record)| {
                                record.is_definition
                                    && use_files.contains(&symbol.file_id)
                                    && (symbol.file_id != use_site.file_id
                                        || symbol.name_range.start_byte < use_site.range.start_byte)
                            })
                            .collect();
                    }
                    let [(symbol, record)] = definitions.as_slice() else {
                        return Err(failure(if definitions.is_empty() {
                            Kind::DefinitionUnavailable
                        } else {
                            Kind::DefinitionAmbiguous
                        }));
                    };
                    if !record.identity_supported {
                        return Err(failure(Kind::LookupRestricted));
                    }
                    if let Some(parameters) = &record.template_parameters {
                        if parameters.len() != ty.template_arguments.len()
                            || self.specialized_templates.get(&name).is_some_and(|files| {
                                files.iter().any(|file| {
                                    *file == reference.file_id || imported.contains(file)
                                })
                            })
                        {
                            return Err(failure(Kind::TemplateUnsupported));
                        }
                    } else if !ty.template_arguments.is_empty() {
                        return Err(failure(Kind::TemplateUnsupported));
                    }
                    return Ok(Some(NamedType::Record(symbol, record)));
                }
                if let Some((first, _)) = ty.name.trim_start_matches("::").split_once("::") {
                    let qualifier = if scope.is_empty() {
                        first.to_string()
                    } else {
                        format!("{scope}::{first}")
                    };
                    if self
                        .names
                        .get(&qualifier)
                        .into_iter()
                        .flatten()
                        .any(|(file, start, _)| {
                            (*file == reference.file_id || imported.contains(file))
                                && (*file != reference.file_id
                                    || *start < reference.range.start_byte)
                        })
                    {
                        // Once a qualifier is found, a missing nested member is
                        // not repaired by choosing an outer same-name qualifier.
                        return Err(failure(Kind::DefinitionUnavailable));
                    }
                }
                // A class search includes inherited declarations before any
                // enclosing-scope fallback. Type lookup may return an inherited
                // class identity; proving that the name is absent is insufficient.
                if let Some(records) = self.records.get(scope) {
                    let owners: Vec<_> = records
                        .iter()
                        .filter(|(symbol, record)| {
                            record.is_definition
                                && (symbol.file_id == reference.file_id
                                    || imported.contains(&symbol.file_id))
                        })
                        .collect();
                    let [(symbol, record)] = owners.as_slice() else {
                        return Err(failure(if owners.is_empty() {
                            Kind::DefinitionUnavailable
                        } else {
                            Kind::DefinitionAmbiguous
                        }));
                    };
                    if let Some(found) =
                        self.inherited_type(symbol, record, ty, reference, path, complete_at)?
                    {
                        return Ok(Some(found));
                    }
                    // A written qualified class definition can be available
                    // while its enclosing declarations are missing. Absence
                    // in this class does not establish absence in that owner.
                    if !members_only
                        && let Some((parent, _)) = scope.rsplit_once("::")
                        && !self.names.get(parent).into_iter().flatten().any(
                            |(file, start, kind)| {
                                (*file == reference.file_id || imported.contains(file))
                                    && (*file != reference.file_id
                                        || *start < reference.range.start_byte)
                                    && matches!(
                                        kind,
                                        SymbolKind::Class
                                            | SymbolKind::Struct
                                            | SymbolKind::Namespace
                                    )
                            },
                        )
                    {
                        return Err(failure(Kind::LookupRestricted));
                    }
                }
                if members_only {
                    return Ok(None);
                }
                if scope.is_empty() {
                    return Err(failure(Kind::DefinitionUnavailable));
                }
                scope = scope.rsplit_once("::").map_or("", |(parent, _)| parent);
            }
        })();
        path.remove(&key);
        result
    }

    // Recover locations from the very declarations used to build the existing
    // restriction indexes. Only reached failures need this small projection;
    // query consumers do not repeat C++ scope/visibility decisions.
    fn lookup_limit_declarations(
        &self,
        scope: &str,
        name: Option<&str>,
        file: FileId,
        visible: &HashSet<FileId>,
    ) -> Vec<(FileId, TextRange)> {
        let files = match name {
            Some(name) => {
                let qualified = if scope.is_empty() {
                    name.to_string()
                } else {
                    format!("{scope}::{name}")
                };
                self.lookup_name_limits.get(&qualified)
            }
            None => self.lookup_limits.get(scope),
        };
        files
            .into_iter()
            .flatten()
            .filter(|candidate| **candidate == file || visible.contains(candidate))
            .flat_map(|file| {
                self.files[file]
                    .lookup_limits
                    .iter()
                    .filter(move |limit| {
                        limit.block_range.is_none()
                            && limit.scope == scope
                            && limit.name.as_deref() == name
                    })
                    .map(move |limit| (*file, limit.declaration_range))
            })
            .collect()
    }

    fn imported_name_declarations(
        &self,
        name: &str,
        file: FileId,
        visible: &HashSet<FileId>,
    ) -> Vec<(FileId, TextRange)> {
        let mut prefix = Some(name);
        while let Some(name) = prefix {
            let (scope, simple) = name.rsplit_once("::").unwrap_or(("", name));
            let declarations = self.lookup_limit_declarations(scope, Some(simple), file, visible);
            if !declarations.is_empty() {
                return declarations;
            }
            prefix = name.rsplit_once("::").map(|(owner, _)| owner);
        }
        Vec::new()
    }

    fn imported_name_limited(&self, name: &str, file: FileId, visible: &HashSet<FileId>) -> bool {
        let mut prefix = Some(name);
        while let Some(name) = prefix {
            if self.lookup_name_limits.get(name).is_some_and(|files| {
                files.contains(&file) || files.iter().any(|file| visible.contains(file))
            }) {
                return true;
            }
            prefix = name.rsplit_once("::").map(|(owner, _)| owner);
        }
        false
    }

    fn block_lookup_limited(&self, name: &str, reference: &ReferenceUse) -> bool {
        self.files.get(&reference.file_id).is_some_and(|file| {
            file.lookup_limits
                .iter()
                .any(|limit| limit.limits_local_lookup(name, reference.range.start_byte))
        })
    }
}

/// Supplemental language-mandated calls at the original member callsite.
/// They do not resolve the written member reference or erase its remaining gap.
pub(crate) fn implicit_calls(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    types: Option<&TypeIndex>,
) -> Vec<ResolvedTarget> {
    let mut implicit = Vec::new();
    if ctx.file.language != Language::Cpp
        || reference.kind != ReferenceKind::Call
        || reference.binding_id.is_some()
        || reference.arity.is_none()
        || !reference
            .text
            .rsplit_once(&reference.name)
            .is_some_and(|(prefix, _)| prefix.trim_end().ends_with("->"))
    {
        return implicit;
    }
    let Some(types) = types else {
        return implicit;
    };
    let Some(owner) = reference
        .source_symbol
        .and_then(|id| ctx.symbols_by_id.get(&id))
    else {
        return implicit;
    };
    if types
        .files
        .get(&reference.file_id)
        .is_none_or(|file| file.unverified_callable_scopes.contains(&owner.id))
    {
        return implicit;
    }
    let owner = owner
        .qualified_name
        .rsplit_once("::")
        .map_or("", |(owner, _)| owner);
    let _ = types.receiver(reference, ctx, owner, &mut implicit, &mut 64);
    implicit
}

pub(crate) fn resolve_call(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    candidates: &[SymbolDef],
    imported_files: &HashSet<FileId>,
    types: Option<&TypeIndex>,
) -> Lookup<ResolvedTarget> {
    resolve_call_with_budget(reference, ctx, candidates, imported_files, types, &mut 64)
}

fn resolve_call_with_budget(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    candidates: &[SymbolDef],
    imported_files: &HashSet<FileId>,
    types: Option<&TypeIndex>,
    deductions_remaining: &mut usize,
) -> Lookup<ResolvedTarget> {
    let normalized;
    let reference = if let Some(types) = types
        && let Some(call) = types.template_call(reference)
    {
        let args = call.arguments.as_ref().ok_or(LookupFailure::Unspecified)?;
        if args.is_empty() {
            return Err(LookupFailure::Unspecified);
        }
        normalized = ReferenceUse {
            name: call.name.clone(),
            text: call.text.clone(),
            receiver: call.receiver.clone(),
            ..reference.clone()
        };
        if normalized.receiver.is_none()
            && types
                .local_value(&normalized.name, &normalized, ctx)?
                .is_some()
        {
            return Err(LookupFailure::Unspecified);
        }
        &normalized
    } else {
        reference
    };
    let target = select_call_with_budget(
        reference,
        ctx,
        candidates,
        imported_files,
        types,
        deductions_remaining,
    )?;
    if let Some(types) = types
        && let Some(symbol) = candidates.iter().find(|s| s.id == target.symbol_id)
    {
        if types.template_call(reference).is_some() {
            templates::check_object(reference, ctx, symbol, types)?;
            types.check_template_argument_types(reference, ctx, symbol, deductions_remaining)?;
        } else {
            types.check_argument_conflicts(reference, ctx, symbol, deductions_remaining)?;
        }
    }
    Ok(target)
}

fn select_call_with_budget(
    reference: &ReferenceUse,
    ctx: &ResolutionContext,
    candidates: &[SymbolDef],
    imported_files: &HashSet<FileId>,
    types: Option<&TypeIndex>,
    deductions_remaining: &mut usize,
) -> Lookup<ResolvedTarget> {
    if reference.source_symbol.is_some_and(|owner| {
        types
            .and_then(|types| types.files.get(&reference.file_id))
            .is_some_and(|file| file.unverified_callable_scopes.contains(&owner))
    }) {
        return Err(LookupFailure::Unspecified);
    }
    let arity = reference.arity.ok_or(LookupFailure::Unspecified)? as usize;
    if let Some(result) = lambdas::resolve(reference, ctx, types) {
        return result;
    }
    // A local/parameter binding can hide a function (e.g. function pointers).
    if reference.binding_id.is_some() {
        return Err(LookupFailure::Unspecified);
    }
    let owner = reference
        .source_symbol
        .and_then(|id| ctx.symbols_by_id.get(&id))
        .and_then(|symbol| {
            symbol
                .qualified_name
                .rsplit_once("::")
                .map(|(owner, _)| owner)
        })
        .unwrap_or("");
    let member_call = reference
        .receiver
        .as_deref()
        .is_some_and(|receiver| reference.text != format!("{receiver}::{}", reference.name));
    if member_call && reference.receiver.as_deref() == Some("this") {
        let types = types.ok_or(LookupFailure::Unspecified)?;
        let visible = types.visible_files(reference.file_id);
        if types
            .files
            .get(&reference.file_id)
            .ok_or(LookupFailure::Unspecified)?
            .this_capture_unavailable
            .contains(&reference.id)
            || !types.records.get(owner).is_some_and(|records| {
                records.iter().any(|(symbol, record)| {
                    record.is_definition
                        && record.lookup_supported
                        && (symbol.file_id == reference.file_id
                            || visible.contains(&symbol.file_id))
                })
            })
        {
            // A namespace cannot supply `this`. A flattened recovery function
            // must not reinterpret this->member as a namespace-level call.
            return Err(LookupFailure::Unspecified);
        }
    }
    if member_call && reference.receiver.as_deref() != Some("this") {
        let types = types.ok_or(LookupFailure::Unspecified)?;
        if types.template_call(reference).is_some()
            && !reference.receiver.as_deref().is_some_and(|receiver| {
                reference.text == format!("{receiver}.{}", reference.name)
                    || reference.text == format!("{receiver}->{}", reference.name)
            })
        {
            return Err(LookupFailure::Unspecified);
        }
        let (record, record_type) =
            types.receiver(reference, ctx, owner, &mut Vec::new(), deductions_remaining)?;
        let visible = types.member_lookup(
            record,
            record_type,
            reference,
            candidates,
            &mut HashSet::new(),
        )?;
        let qname = visible
            .first()
            .ok_or(LookupFailure::Unspecified)?
            .qualified_name
            .clone();
        if types.template_call(reference).is_none()
            && !types.nonvirtual_named(
                record,
                record_type,
                reference,
                candidates,
                &mut HashSet::new(),
            )?
        {
            return Err(LookupFailure::Unspecified);
        }
        if visible.iter().any(|symbol| {
            types
                .written_callable(symbol)
                .is_none_or(|callable| callable.is_virtual || callable.qualifiers.contains('&'))
        }) {
            return Err(LookupFailure::Unspecified);
        }
        return choose_target(reference, &qname, &visible, candidates, arity, types);
    }
    let written = if member_call {
        if owner.is_empty() {
            return Err(LookupFailure::Unspecified);
        }
        reference.name.clone()
    } else if let Some(receiver) = &reference.receiver {
        format!("{receiver}::{}", reference.name)
    } else {
        reference.text.clone()
    };
    if !written
        .trim_start_matches("::")
        .split("::")
        .all(plain_identifier)
    {
        return Err(LookupFailure::Unspecified);
    }

    let absolute = written.starts_with("::");
    if !member_call
        && let Some(types) = types
        && types.block_lookup_limited(&written, reference)
    {
        let declarations = types.files[&reference.file_id]
            .lookup_limits
            .iter()
            .filter(|limit| limit.limits_local_lookup(&written, reference.range.start_byte))
            .map(|limit| (reference.file_id, limit.declaration_range))
            .collect();
        return Err(LookupFailure::name_restricted(
            &written,
            owner,
            reference,
            declarations,
        ));
    }
    let unqualified = !absolute && (member_call || reference.receiver.is_none());
    let visible_files = unqualified
        .then(|| types.map(|types| types.visible_files(reference.file_id)))
        .flatten();
    let imported_files = visible_files.as_ref().unwrap_or(imported_files);
    let mut scope = if absolute { "" } else { owner };
    loop {
        let qname = if scope.is_empty() {
            written.trim_start_matches("::").to_string()
        } else {
            format!("{scope}::{written}")
        };
        if let Some(types) = types {
            if types.imported_name_limited(&qname, reference.file_id, imported_files) {
                return Err(LookupFailure::name_restricted(
                    &written,
                    scope,
                    reference,
                    types.imported_name_declarations(&qname, reference.file_id, imported_files),
                ));
            }
            if unqualified
                && types.lookup_limits.get(scope).is_some_and(|files| {
                    files
                        .iter()
                        .any(|file| *file == reference.file_id || imported_files.contains(file))
                })
            {
                return Err(LookupFailure::name_restricted(
                    &written,
                    scope,
                    reference,
                    types.lookup_limit_declarations(scope, None, reference.file_id, imported_files),
                ));
            }
        }
        let visible: Vec<_> = candidates.iter().filter(|s| {
            s.language == Language::Cpp && s.qualified_name == qname
                && (s.file_id == reference.file_id || imported_files.contains(&s.file_id))
                // A later free-function definition is not a visible declaration.
                && (s.file_id != reference.file_id || s.name_range.start_byte <= reference.range.start_byte
                    || (s.kind == SymbolKind::Method && (member_call || reference.receiver.is_none())
                        && s.qualified_name.rsplit_once("::").is_some_and(|(parent, _)| parent == owner)))
        }).collect();
        if !visible.is_empty() {
            let types = types.ok_or(LookupFailure::Unspecified)?;
            if unqualified
                && !member_call
                && !types.is_member_set(&qname, &visible, reference.file_id)
            {
                return types.free_call(reference, ctx, &visible, candidates, deductions_remaining);
            }
            return choose_target(reference, &qname, &visible, candidates, arity, types);
        }
        if unqualified {
            let types = types.ok_or(LookupFailure::Unspecified)?;
            if let Some(records) = types.records.get(scope) {
                let records: Vec<_> = records
                    .iter()
                    .filter(|(symbol, record)| {
                        record.is_definition
                            && (symbol.file_id == reference.file_id
                                || imported_files.contains(&symbol.file_id))
                    })
                    .collect();
                let [(record, record_type)] = records.as_slice() else {
                    return Err(LookupFailure::Unspecified);
                };
                // Dependent scopes require instantiation and template-parameter
                // lookup before proving that a name is absent.
                if record_type.template_parameters.is_some() {
                    return Err(LookupFailure::Unspecified);
                }
                let members = types.member_lookup(
                    record,
                    record_type,
                    reference,
                    candidates,
                    &mut HashSet::new(),
                )?;
                if let Some(first) = members.first() {
                    if members.iter().any(|symbol| {
                        types
                            .callable(symbol)
                            .is_none_or(|callable| callable.qualifiers.contains('&'))
                    }) {
                        return Err(LookupFailure::Unspecified);
                    }
                    return choose_target(
                        reference,
                        &first.qualified_name,
                        &members,
                        candidates,
                        arity,
                        types,
                    );
                }
                // An empty callable set is not absence of the name: an own or
                // inherited field/alias can hide a namespace-level function.
                if types.has_member_name(record, &reference.name)
                    || !types.inherited_name_absent(
                        record,
                        record_type,
                        &reference.name,
                        reference,
                        &mut HashSet::new(),
                    )?
                {
                    return Err(LookupFailure::Unspecified);
                }
            } else if !scope.is_empty()
                && !types.names.get(scope).is_some_and(|names| {
                    names.iter().any(|(file, _, kind)| {
                        *kind == SymbolKind::Namespace
                            && (*file == reference.file_id || imported_files.contains(file))
                    })
                })
            {
                // A missing/recovered class is not evidence of a namespace.
                return Err(LookupFailure::Unspecified);
            }
            // Ordinary lookup stops at the nearest name; associated lookup is
            // evaluated with the actual arguments once that lookup completes.
            if member_call {
                return Err(LookupFailure::Unspecified);
            }
            let caller = ctx
                .symbols_by_id
                .get(&reference.source_symbol.ok_or(LookupFailure::Unspecified)?)
                .ok_or(LookupFailure::Unspecified)?;
            // A closure has a lexical lookup context even though it is not an
            // ordinary named declaration. Its scope validity was checked above;
            // invocation/parameter support must not erase body-local facts.
            if types.callable(caller).is_none()
                && !types.files.get(&reference.file_id).is_some_and(|file| {
                    file.lambda_captures
                        .iter()
                        .any(|lambda| lambda.symbol_id == Some(caller.id))
                })
            {
                return Err(LookupFailure::Unspecified);
            }
            let parent = scope.rsplit_once("::").map_or("", |(parent, _)| parent);
            if types.records.contains_key(parent) {
                // Nested-class object/lookup semantics are not modeled here.
                return Err(LookupFailure::Unspecified);
            }
        }
        if absolute || scope.is_empty() {
            if unqualified && !member_call {
                return types.ok_or(LookupFailure::Unspecified)?.free_call(
                    reference,
                    ctx,
                    &[],
                    candidates,
                    deductions_remaining,
                );
            }
            return Err(LookupFailure::Unspecified);
        }
        scope = scope
            .rsplit_once("::")
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
}

fn plain_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn choose_target(
    reference: &ReferenceUse,
    qname: &str,
    visible: &[&SymbolDef],
    candidates: &[SymbolDef],
    arity: usize,
    types: &TypeIndex,
) -> Lookup<ResolvedTarget> {
    if types.template_call(reference).is_some() {
        return templates::choose(reference, visible, candidates, arity, types);
    }
    // Associated lookup is handled by the free-call entry using actual arguments.
    // This shared stage only checks signature identity and declaration/body
    // association; template spelling is not a substitute for ADL applicability.
    // Lookup terminates at the first visible name. Unknown signatures, hiding,
    // and multiple applicable overloads cannot be repaired by a wider search.
    let mut applicable: BTreeMap<(Vec<String>, String), Vec<&SymbolDef>> = BTreeMap::new();
    for symbol in visible {
        if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            return Err(LookupFailure::Unspecified);
        }
        let callable = types.callable(symbol).ok_or(LookupFailure::Unspecified)?;
        if callable.minimum_arity as usize <= arity && arity <= callable.parameter_types.len() {
            applicable
                .entry((
                    callable.parameter_types.clone(),
                    callable.qualifiers.clone(),
                ))
                .or_default()
                .push(symbol);
        }
    }
    if applicable.len() != 1 {
        return Err(LookupFailure::Unspecified);
    }
    let (identity, declarations) = applicable
        .into_iter()
        .next()
        .ok_or(LookupFailure::Unspecified)?;
    let internal_linkage = declarations
        .iter()
        .any(|s| types.callable(s).is_some_and(|c| c.internal_linkage));
    let imported = types.visible_files(reference.file_id);
    let class_member = declarations.iter().any(|s| {
        s.kind == SymbolKind::Method
            || types
                .callable(s)
                .is_some_and(|callable| !callable.qualifiers.is_empty())
    }) || qname.rsplit_once("::").is_some_and(|(owner, _)| {
        types.records.get(owner).is_some_and(|records| {
            records
                .iter()
                .any(|(s, _)| s.file_id == reference.file_id || imported.contains(&s.file_id))
        })
    });
    if (reference.receiver.is_none() || reference.receiver.as_deref() == Some("this"))
        && class_member
        && !declarations.iter().any(|s| s.static_)
    {
        if types
            .files
            .get(&reference.file_id)
            .ok_or(LookupFailure::Unspecified)?
            .this_capture_unavailable
            .contains(&reference.id)
        {
            // Finding a non-static member does not establish a captured object.
            // Explicit this captures are already distinguished by extraction.
            return Err(LookupFailure::Unspecified);
        }
        // `f()` and `this->f()` dispatch like an explicit object call. An
        // override remains virtual even when it omits virtual/override itself;
        // checking only the selected declaration could wrongly enter its body.
        let owner = qname.rsplit_once("::").ok_or(LookupFailure::Unspecified)?.0;
        let records: Vec<_> = types
            .records
            .get(owner)
            .ok_or(LookupFailure::Unspecified)?
            .iter()
            .filter(|(s, r)| {
                r.is_definition && (s.file_id == reference.file_id || imported.contains(&s.file_id))
            })
            .collect();
        let [(record, record_type)] = records.as_slice() else {
            return Err(LookupFailure::Unspecified);
        };
        if !types.nonvirtual_named(
            record,
            record_type,
            reference,
            candidates,
            &mut HashSet::new(),
        )? {
            return Err(LookupFailure::Unspecified);
        }
    }
    // Do not select a runtime override for an implicit/this virtual call.
    let virtual_call = !reference
        .receiver
        .as_deref()
        .is_some_and(|receiver| reference.text == format!("{receiver}::{}", reference.name))
        && declarations
            .iter()
            .any(|s| types.callable(s).is_some_and(|c| c.is_virtual));
    let visible_bodies: Vec<_> = declarations
        .iter()
        .copied()
        .filter(|s| s.range != s.name_range)
        .collect();
    let bodies: Vec<_> = if virtual_call {
        Vec::new()
    } else if !visible_bodies.is_empty() {
        visible_bodies
    } else {
        candidates.iter().filter(|s| {
            s.language == Language::Cpp && s.qualified_name == qname
                && matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                // C++ extraction expands definitions to the function scope;
                // file/namespace prototypes and class declarations retain the name range.
                && s.range != s.name_range
                && types.callable(s).is_some_and(|c| c.parameter_types == identity.0 && c.qualifiers == identity.1)
                && ((!internal_linkage && types.callable(s).is_some_and(|c| !c.internal_linkage)) || s.file_id == reference.file_id)
                && declarations.iter().any(|declaration| types.can_associate(declaration, s, &identity.0))
        }).collect()
    };
    let target = match bodies.as_slice() {
        [body] => *body,
        [] => match declarations.as_slice() {
            [declaration] => *declaration,
            _ => return Err(LookupFailure::Unspecified),
        },
        _ => return Err(LookupFailure::Unspecified),
    };
    Ok(ResolvedTarget {
        symbol_id: target.id,
        confidence: Confidence::certain(),
        strategy: ResolutionStrategy::ExactMatch,
        provenance: Provenance::TreeSitter,
    })
}
