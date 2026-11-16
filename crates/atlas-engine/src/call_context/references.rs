//! Read-only uses of an indexed lexical declaration, without value propagation.
use super::*;

#[derive(Debug, Default)]
pub struct BindingReferences {
    pub declaration: Option<ContextLocation>,
    pub name: Option<String>,
    pub scope: Option<ContextLocation>,
    pub references: Vec<ContextLocation>,
    pub candidates: Vec<(ContextLocation, String)>,
    pub gaps: Vec<(ContextLocation, &'static str, String)>,
    pub files_read: usize,
    pub bytes_read: usize,
}

/// The selector is a declaration name or its exact declaration syntax range.
/// Only local/parameter lexical bindings are supported. Unknown uses remain
/// located candidates; nothing is written to the reference table or call graph.
pub fn find_binding_references(
    store: &Store,
    root: &Path,
    path: &str,
    start: u32,
    end: u32,
    canceled: &dyn Fn() -> bool,
) -> anyhow::Result<BindingReferences> {
    let mut result = BindingReferences::default();
    let mut query = Investigation {
        store,
        root,
        canceled,
        parsed: BTreeMap::new(),
        result: CallContextResult::default(),
        symbol_static: BTreeMap::new(),
        reusable: None,
        admitted_bytes: 0,
    };
    query.check()?;
    let Some(file) = store
        .find_files_by_path_prefix(path)?
        .into_iter()
        .find(|f| f.path == path)
    else {
        anyhow::bail!("reference declaration file is not indexed");
    };
    let requested = ContextLocation {
        file_id: file.file_id,
        range: TextRange {
            start_byte: start,
            end_byte: end,
            ..Default::default()
        },
    };
    if file.language != Language::Cpp {
        result.gaps.push((requested,"reference_language_unsupported","Lexical reference lookup is currently implemented for C++ local variables and parameters; use text search for other declarations.".into()));
        return Ok(result);
    }
    let parsed = match query.source(file.file_id) {
        Ok(parsed) => parsed,
        Err(error) => {
            result
                .gaps
                .push((requested, "source_context_unavailable", error));
            result.files_read = query.result.files_read;
            result.bytes_read = query.result.bytes_read;
            return Ok(result);
        }
    };
    let bindings = store.find_bindings_by_file(&file.file_id)?;
    let selected: Vec<_> = bindings
        .iter()
        .filter(|binding| {
            (binding.range.start_byte == start && binding.range.end_byte == end)
                || cpp::binding_origin(parsed.tree.root_node(), binding.range).is_some_and(
                    |origin| {
                        origin.declaration.start_byte() == start as usize
                            && origin.declaration.end_byte() == end as usize
                    },
                )
        })
        .collect();
    if selected.len() != 1 {
        result.gaps.push((requested,"reference_declaration_unavailable","Select one indexed local variable or parameter by its name or declaration range; functions, fields and project-wide symbol references are not implemented by this operation yet.".into()));
    } else {
        let binding = selected[0];
        result.name = Some(binding.name.clone());
        result.declaration = Some(ContextLocation {
            file_id: file.file_id,
            range: binding.range,
        });
        let scopes: BTreeMap<_, _> = store
            .find_scopes_by_file(&file.file_id)?
            .into_iter()
            .map(|s| (s.id, s))
            .collect();
        let scope = scopes.get(&binding.scope_id);
        if scope.is_none_or(|scope| {
            !matches!(
                scope.kind,
                ScopeKind::Function | ScopeKind::Method | ScopeKind::Block
            )
        }) {
            result.gaps.push((requested,"reference_scope_unsupported","Only a local or parameter declaration in a callable/block scope can be enumerated; keep text search available for other scopes.".into()));
        } else if let Some(node) =
            scope.and_then(|scope| cpp::expression(parsed.tree.root_node(), scope.range))
        {
            result.scope = Some(loc(file.file_id, node));
            scan(
                &mut query,
                &mut result,
                &parsed,
                node,
                binding,
                &bindings,
                &scopes,
            )?;
            result.gaps.push((loc(file.file_id,node),"lexical_reference_coverage_limited","Examined written identifier syntax in this lexical scope. Macro expansion, aliases, runtime values and uses through other objects are not enumerated; text search remains independently available.".into()));
        } else {
            result.gaps.push((requested,"reference_scope_unavailable","The indexed lexical scope cannot be matched to current parser syntax; inspect its source.".into()));
        }
    }
    query.check()?;
    result.files_read = query.result.files_read;
    result.bytes_read = query.result.bytes_read;
    Ok(result)
}

fn scan(
    query: &mut Investigation<'_>,
    result: &mut BindingReferences,
    parsed: &ParsedSource,
    root: Node<'_>,
    selected: &BindingDef,
    bindings: &[BindingDef],
    scopes: &BTreeMap<ScopeId, ScopeDef>,
) -> anyhow::Result<()> {
    let declarations: BTreeSet<_> = bindings
        .iter()
        .map(|b| (b.range.start_byte, b.range.end_byte))
        .collect();
    let symbols = query.store.find_symbols_by_file(&selected.file_id)?;
    let symbol_names: BTreeSet<_> = symbols
        .iter()
        .map(|s| (s.name_range.start_byte, s.name_range.end_byte))
        .collect();
    let closures = parsed
        .cpp
        .as_ref()
        .map_or(&[][..], |facts| facts.lambda_captures.as_slice());
    let mut cursor = root.walk();
    loop {
        query.check()?;
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            result.gaps.push((loc(selected.file_id,node),"reference_syntax_unexamined","Parser recovery at this region may omit identifier uses; continue with source/text search.".into()));
        }
        if node.kind() == "identifier" && cpp::text(node, &parsed.source) == selected.name {
            let at = loc(selected.file_id, node);
            let key = (at.range.start_byte, at.range.end_byte);
            let parent = node.parent().map(|n| n.kind());
            if !declarations.contains(&key)
                && !symbol_names.contains(&key)
                && !matches!(
                    parent,
                    Some("qualified_identifier" | "goto_statement" | "labeled_statement")
                )
            {
                let scope = scopes
                    .values()
                    .filter(|s| {
                        s.range.start_byte <= at.range.start_byte
                            && s.range.end_byte >= at.range.end_byte
                    })
                    .min_by_key(|s| s.range.byte_len())
                    .map(|s| s.id);
                let found = visible_bindings(
                    bindings,
                    scopes,
                    scope,
                    &selected.name,
                    at.range.start_byte,
                    closures,
                );
                // A simple capture names an existing binding in the enclosing
                // scope. Init/pack captures are different declaration forms;
                // use the shared extracted capture inventory to distinguish them.
                let ordinary_capture = node.parent().is_some_and(|captures| {
                    captures.kind() == "lambda_capture_specifier"
                        && captures.parent().is_some_and(|lambda| {
                            closures.iter().any(|record| {
                                record.range == cpp::range(lambda)
                                    && record.captures.as_ref().is_some_and(|names| {
                                        names.iter().any(|(name, kind)| {
                                            name == &selected.name
                                                && matches!(
                                                    kind,
                                                    types::cpp::CppCaptureKind::Copy
                                                        | types::cpp::CppCaptureKind::Reference
                                                )
                                        })
                                    })
                            })
                        })
                });
                let syntax_unverified = std::iter::successors(Some(node), |n| n.parent())
                    .take_while(|n| {
                        *n != root
                            && !matches!(
                                n.kind(),
                                "compound_statement" | "lambda_expression" | "function_definition"
                            )
                    })
                    .any(|n| {
                        n.is_error()
                            || n.is_missing()
                            || matches!(
                                n.kind(),
                                "array_declarator"
                                    | "function_declarator"
                                    | "structured_binding_declarator"
                            )
                            || (n.kind() == "lambda_capture_specifier" && !ordinary_capture)
                            || parsed.cpp.as_ref().is_some_and(|facts| {
                                let spelling = if n.kind() == "call_expression" {
                                    n.child_by_field_name("function")
                                        .map(|callee| {
                                            callee
                                                .child_by_field_name("field")
                                                .or_else(|| callee.child_by_field_name("name"))
                                                .unwrap_or(callee)
                                        })
                                        .map(|callee| cpp::text(callee, &parsed.source))
                                } else if n.kind() == "identifier" {
                                    Some(cpp::text(n, &parsed.source))
                                } else {
                                    None
                                };
                                spelling.is_some_and(|name| {
                                    facts
                                        .macros
                                        .iter()
                                        .any(|definition| definition.name == name)
                                })
                            })
                    });
                let mut nested_scope = scope;
                let mut scope_unverified = parsed.cpp.as_ref().is_some_and(|facts| {
                    facts
                        .lookup_limits
                        .iter()
                        .any(|limit| limit.limits_local_lookup(&selected.name, at.range.start_byte))
                });
                let mut visited = BTreeSet::new();
                while let Some(id) = nested_scope {
                    query.check()?;
                    if !visited.insert(id) {
                        scope_unverified = true;
                        break;
                    }
                    if id == selected.scope_id {
                        break;
                    }
                    let Some(scope) = scopes.get(&id) else {
                        scope_unverified = true;
                        break;
                    };
                    if matches!(
                        scope.kind,
                        ScopeKind::Class | ScopeKind::Struct | ScopeKind::Namespace
                    ) {
                        scope_unverified = true;
                        break;
                    }
                    nested_scope = scope.parent_id;
                }
                let capture_verified = !cpp::inside_lambda(node)
                    || parsed.cpp.as_ref().is_some_and(|facts| {
                        facts
                            .values
                            .iter()
                            .find(|value| value.binding_id == Some(selected.id))
                            .is_some_and(|value| {
                                let reference = ReferenceUse {
                                    id: ReferenceId::generate(
                                        &selected.file_id,
                                        None,
                                        at.range.start_byte,
                                        at.range.end_byte,
                                        &selected.name,
                                        ReferenceKind::Usage,
                                    ),
                                    file_id: selected.file_id,
                                    source_symbol: None,
                                    scope_id: scope,
                                    kind: ReferenceKind::Usage,
                                    text: selected.name.clone(),
                                    name: selected.name.clone(),
                                    receiver: None,
                                    arity: None,
                                    range: at.range,
                                    binding_id: None,
                                    resolved: None,
                                };
                                resolution::cpp_captures::local_effect(
                                    facts,
                                    &selected.name,
                                    value,
                                    &reference,
                                )
                                .is_some_and(|effect| {
                                    // Capturing an outer closure's copy requires
                                    // its member identity, not the original local.
                                    // Static storage also cannot be a simple capture.
                                    !ordinary_capture || (value.capture_required && !effect.copy)
                                })
                            })
                    });
                if !syntax_unverified
                    && !scope_unverified
                    && found.len() == 1
                    && found[0].id == selected.id
                    && capture_verified
                {
                    result.references.push(at);
                } else if syntax_unverified
                    || scope_unverified
                    || found.len() != 1
                    || !capture_verified
                {
                    result.candidates.push((at,"Written identifier with this name; scope, declaration syntax or capture prerequisites do not establish a unique binding to the selected declaration.".into()));
                }
                // A uniquely identified nearer binding shadows this declaration.
            }
        }
        anyhow::ensure!(
            result.references.len() + result.candidates.len() + result.gaps.len()
                < MAX_CONTEXT_ITEMS,
            "reference investigation exceeds 10000 records; select a smaller declaration scope"
        );
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
}

#[cfg(all(test, feature = "cpp"))]
mod tests {
    use super::*;
    use crate::{ExtractionMode, IndexPipeline, IndexPipelineOptions, NoopSink};
    #[test]
    fn explicit_captures_refer_to_visible_bindings_without_flattening_capture_values() {
        let source = r#"void consume(int);
void entry(int task) {
    auto copied = [task] { consume(task); };
    auto referenced = [&task] { consume(task); };
    auto renamed = [alias = task] { consume(alias); };
    auto replaced = [task = 7] { consume(task); };
    { int task = 3; auto shadowed = [&task] { consume(task); }; }
    auto chain = [&task] { auto inner = [&task] { consume(task); }; };
    auto copied_chain = [task] { auto inner = [&task] { consume(task); }; };
}"#;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.cpp"), source).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        IndexPipeline::new(
            store.clone(),
            root.path().to_path_buf(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap();
        let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
        let start = source.find("int task").unwrap() as u32 + 4;
        let result =
            find_binding_references(&store, root.path(), "main.cpp", start, start + 4, &|| false)
                .unwrap();
        let mut expected = Vec::new();
        for line in [
            "    auto copied =",
            "    auto referenced =",
            "    auto chain =",
        ] {
            let at = source.find(line).unwrap();
            let text = source[at..].lines().next().unwrap();
            expected.extend(
                text.match_indices("task")
                    .map(|(offset, _)| (at + offset) as u32),
            );
        }
        expected
            .push((source.find("copied_chain = [task").unwrap() + "copied_chain = [".len()) as u32);
        assert_eq!(
            result
                .references
                .iter()
                .map(|at| at.range.start_byte)
                .collect::<Vec<_>>(),
            expected,
            "{result:#?}"
        );
        // Initializer references and the next capture through a copied closure
        // need facts beyond the supported lexical binding/capture model.
        for spelling in ["alias = task", "inner = [&task"] {
            let at = source.rfind(spelling).unwrap() + spelling.len() - 4;
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|(loc, _)| loc.range.start_byte as usize == at),
                "{result:#?}"
            );
        }
        assert_eq!(
            serde_json::to_value(store.get_all_edges().unwrap()).unwrap(),
            before
        );
    }

    #[test]
    fn unexpanded_local_import_does_not_bind_to_an_outer_homonym() {
        let source = "namespace other { int task; } void consume(int); void entry(int task) { consume(task); { using other::task; consume(task); } consume(task); }";
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.cpp"), source).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        IndexPipeline::new(
            store.clone(),
            root.path().to_path_buf(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap();
        let start = source.find("entry(int task").unwrap() as u32 + 10;
        let result =
            find_binding_references(&store, root.path(), "main.cpp", start, start + 4, &|| false)
                .unwrap();
        assert_eq!(
            result
                .references
                .iter()
                .map(|at| at.range.start_byte as usize)
                .collect::<Vec<_>>(),
            [
                source.find("consume(task)").unwrap() + 8,
                source.rfind("consume(task)").unwrap() + 8
            ],
            "{result:#?}"
        );
        assert!(
            result
                .candidates
                .iter()
                .any(|(at, _)| at.range.start_byte as usize
                    == source.match_indices("consume(task)").nth(1).unwrap().0 + 8)
        );
    }
    #[test]
    fn initializer_uses_remain_visible_while_macros_and_qualified_names_do_not_bind() {
        let source = "#define TEXT(x) #x\n#define DROP(x) skip()\nvoid skip(); void consume(int); struct Receiver { int task; void skip(); }; namespace other { int task; } void entry(int task) { TEXT(task); Receiver r; r.DROP(task); auto copy = task; (void)sizeof(task); (void)other::task; (void)r.task; { consume(task); int task=3; consume(task); } }";
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.cpp"), source).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        IndexPipeline::new(
            store.clone(),
            root.path().to_path_buf(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap();
        let start = source.find("entry(int task").unwrap() as u32 + 10;
        let result =
            find_binding_references(&store, root.path(), "main.cpp", start, start + 4, &|| false)
                .unwrap();
        assert_eq!(
            result
                .references
                .iter()
                .map(|at| at.range.start_byte as usize)
                .collect::<Vec<_>>(),
            [
                source.find("copy = task").unwrap() + 7,
                source.find("sizeof(task)").unwrap() + 7,
                source.find("consume(task)").unwrap() + 8
            ],
            "{result:#?}"
        );
        for text in ["TEXT(task)", "DROP(task)"] {
            let start = source.find(text).unwrap() + 5;
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|(at, _)| at.range.start_byte as usize == start),
                "{text}: {result:#?}"
            );
        }
    }
    #[test]
    fn local_references_preserve_assignment_and_separate_shadowing_and_capture_unknowns() {
        let source = "void consume(int); void entry(int task) { consume(task); task = 2; { int task = 3; consume(task); } auto yes = [&task] { consume(task); }; auto no = [] { consume(task); }; auto replaced = [task = 4] { consume(task); }; consume(task); }";
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.cpp"), source).unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        IndexPipeline::new(
            store.clone(),
            root.path().to_path_buf(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap();
        let start = source.find("task)").unwrap() as u32;
        let result =
            find_binding_references(&store, root.path(), "main.cpp", start, start + 4, &|| false)
                .unwrap();
        let positions: Vec<_> = result
            .references
            .iter()
            .map(|at| at.range.start_byte)
            .collect();
        let expected = vec![
            source.find("consume(task)").unwrap() as u32 + 8,
            source.find("task = 2").unwrap() as u32,
            source.find("[&task]").unwrap() as u32 + 2,
            source.find("consume(task); };").unwrap() as u32 + 8,
            source.rfind("consume(task)").unwrap() as u32 + 8,
        ];
        assert_eq!(positions, expected, "{result:#?}");
        assert!(!result.candidates.is_empty());
        assert!(
            find_binding_references(&store, root.path(), "main.cpp", start, start + 4, &|| true)
                .is_err()
        );
    }
}
