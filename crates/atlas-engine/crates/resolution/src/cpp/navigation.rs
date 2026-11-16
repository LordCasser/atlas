//! Read-only declaration navigation using the resolver's existing identity rules.
//! Base identity, member declarations and available bodies are separate facts;
//! none of them establishes a virtual call's runtime target.
use super::*;

#[derive(Debug, Clone)]
pub struct CppBaseNavigation {
    pub written: types::cpp::CppBaseClass,
    pub target: Option<SymbolDef>,
    pub failure: Option<types::cpp::CppTypeLookupFailure>,
}

#[derive(Debug, Clone)]
pub struct CppMemberNavigation {
    pub declaration: SymbolDef,
    pub callable: Option<CppCallableDeclaration>,
    /// All matching bodies remain visible. More than one is not a unique definition.
    pub definitions: Vec<SymbolDef>,
}

#[derive(Debug, Clone)]
pub struct CppRecordNavigation {
    pub declaration: SymbolDef,
    pub identity_supported: bool,
    pub lookup_supported: bool,
    /// None retains an unexamined/unsupported base clause.
    pub bases: Option<Vec<CppBaseNavigation>>,
    pub members: Vec<CppMemberNavigation>,
}

pub fn cpp_declaration_navigation(
    store: &db::Store,
    interrupted: &dyn Fn() -> bool,
) -> anyhow::Result<Vec<CppRecordNavigation>> {
    let check = || -> anyhow::Result<()> {
        anyhow::ensure!(!interrupted(), "C++ declaration navigation interrupted");
        Ok(())
    };
    check()?;
    let files = store.all_cpp_types()?;
    let mut symbols = Vec::new();
    for file in files.keys() {
        check()?;
        symbols.extend(store.find_symbols_by_file(file)?);
    }
    symbols.sort_by_key(|symbol| symbol.id);
    let index = TypeIndex::from_symbols(&symbols, files, store)?;
    check()?;
    let mut members: HashMap<SymbolId, Vec<&SymbolDef>> = HashMap::new();
    let mut bodies: HashMap<&str, Vec<&SymbolDef>> = HashMap::new();
    for symbol in &symbols {
        check()?;
        if !matches!(
            symbol.kind,
            SymbolKind::Method | SymbolKind::Function | SymbolKind::Constructor
        ) {
            continue;
        }
        if let Some(owner) = symbol.container {
            members.entry(owner).or_default().push(symbol);
        }
        if symbol.range != symbol.name_range {
            bodies
                .entry(&symbol.qualified_name)
                .or_default()
                .push(symbol);
        }
    }
    let mut result = Vec::new();
    for symbol in &symbols {
        check()?;
        let Some(record) = index.files.get(&symbol.file_id).and_then(|file| {
            file.records
                .iter()
                .find(|record| record.symbol_id == symbol.id)
        }) else {
            continue;
        };
        if !record.is_definition {
            continue;
        }
        let mut bases = record.bases.as_ref().map(|_| Vec::new());
        for base in record.bases.iter().flatten() {
            check()?;
            let site = ReferenceUse {
                id: ReferenceId::default(),
                file_id: symbol.file_id,
                source_symbol: Some(symbol.id),
                scope_id: symbol.scope_id,
                kind: ReferenceKind::Inheritance,
                text: base.declared_type.name.clone(),
                name: base.declared_type.name.clone(),
                receiver: None,
                arity: None,
                range: base.range,
                binding_id: None,
                resolved: None,
            };
            let (target, failure) = if record.identity_supported {
                match index.base_record(symbol, base, &site) {
                    Ok((target, _)) => (Some(target.clone()), None),
                    Err(LookupFailure::Type(failure)) => (None, Some(*failure)),
                    Err(LookupFailure::Unspecified) => (None, None),
                }
            } else {
                (None, None)
            };
            bases.as_mut().unwrap().push(CppBaseNavigation {
                written: base.clone(),
                target,
                failure,
            });
        }
        let mut declared = Vec::new();
        for member in members.get(&symbol.id).into_iter().flatten() {
            check()?;
            // Out-of-line definitions may have a semantic class container. Only
            // members written inside this class are declaration roots here.
            if member.file_id != symbol.file_id
                || member.range.start_byte < symbol.range.start_byte
                || member.range.end_byte > symbol.range.end_byte
            {
                continue;
            }
            let callable = index.written_callable(member).cloned();
            let mut definitions = Vec::new();
            if callable.is_some() {
                if member.range != member.name_range {
                    definitions.push((*member).clone());
                } else {
                    for body in bodies
                        .get(member.qualified_name.as_str())
                        .into_iter()
                        .flatten()
                    {
                        check()?;
                        if index.matches_selected_declaration(member, body) {
                            definitions.push((*body).clone());
                        }
                    }
                }
            }
            declared.push(CppMemberNavigation {
                declaration: (*member).clone(),
                callable,
                definitions,
            });
        }
        result.push(CppRecordNavigation {
            declaration: symbol.clone(),
            identity_supported: record.identity_supported,
            lookup_supported: record.lookup_supported,
            bases,
            members: declared,
        });
    }
    Ok(result)
}
