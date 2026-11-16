#![cfg(feature = "cpp")]

use atlas_engine::{
    CompilerBindingGap, CompilerBindingGapReason, CompilerCallInput, CompilerDeclaration,
    CompilerDispatch, CompilerLocation, CompilerObservation, CompilerObservationKind,
    ExtractionMode, GraphBuilder, IndexPipeline, IndexPipelineOptions, KEY_COMPILER_CALL_GAPS,
    NoopSink, PhaseName, ProgressEvent, ProgressSink, Provenance, ResolutionStrategy, Store,
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const SOURCE: &str = "namespace Left { int target() { return 1; } } namespace Right { int target() { return 2; } } int helper() { return 3; } int selected() { return Left::target(); } int independent() { return helper(); }";

fn location(source: &str, text: &str) -> CompilerLocation {
    let start = source.find(text).unwrap();
    CompilerLocation {
        path: "sample.cpp".into(),
        start_byte: start as u32,
        end_byte: (start + text.len()) as u32,
    }
}

fn input() -> CompilerCallInput {
    let target = CompilerDeclaration {
        name: "target".into(),
        kind: atlas_engine::SymbolKind::Function,
        qualified_name: "Left::target".into(),
        location: location(SOURCE, "target"),
    };
    let mut at = location(SOURCE, "Left::target()");
    at.start_byte += "Left::".len() as u32;
    at.end_byte -= 2;
    CompilerCallInput {
        inputs: BTreeMap::from([(
            "sample.cpp".into(),
            blake3::hash(SOURCE.as_bytes()).to_hex().to_string(),
        )]),
        observations: vec![CompilerObservation {
            kind: CompilerObservationKind::CallDeclaration,
            location: at,
            call_expression: Some(location(SOURCE, "Left::target()")),
            declaration: target.clone(),
            definition: Some(target),
            owner: Some(CompilerDeclaration {
                name: "selected".into(),
                kind: atlas_engine::SymbolKind::Function,
                qualified_name: "selected".into(),
                location: location(SOURCE, "selected"),
            }),
            dispatch: CompilerDispatch::Direct,
        }],
    }
}

fn setup() -> (tempfile::TempDir, Arc<Store>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("sample.cpp"), SOURCE).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    (root, store)
}

fn pipeline(store: &Arc<Store>, root: &Path, compiler: Option<CompilerCallInput>) -> IndexPipeline {
    let mut options = IndexPipelineOptions::new(ExtractionMode::Structural);
    options.compiler_calls = compiler;
    IndexPipeline::new(store.clone(), root.into(), options)
}

fn run(
    store: &Arc<Store>,
    root: &Path,
    compiler: Option<CompilerCallInput>,
) -> atlas_engine::IndexPipelineStats {
    pipeline(store, root, compiler)
        .run(&NoopSink, &mut || false)
        .unwrap()
}

fn calls(store: &Store) -> BTreeMap<String, serde_json::Value> {
    store
        .get_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind.as_str() == "calls")
        .map(|e| (e.id.to_hex(), serde_json::to_value(e).unwrap()))
        .collect()
}

fn gaps(store: &Store) -> Vec<CompilerBindingGap> {
    serde_json::from_str(&store.get_metadata(KEY_COMPILER_CALL_GAPS).unwrap().unwrap()).unwrap()
}

#[test]
fn compiler_selection_replaces_old_primary_edges_and_source_only_can_be_restored() {
    let (root, store) = setup();
    run(&store, root.path(), None);
    let baseline = calls(&store);
    assert_eq!(baseline.len(), 2);
    let mut refs = store.get_all_call_references().unwrap();
    let selected = refs
        .iter_mut()
        .find(|r| {
            r.range.start_byte > SOURCE.find("return Left::").unwrap() as u32
                && r.range.start_byte < SOURCE.find("int independent").unwrap() as u32
        })
        .unwrap();
    let selected_id = selected.id;
    let wrong = store
        .get_all_symbols()
        .unwrap()
        .into_iter()
        .find(|s| s.qualified_name == "Right::target")
        .unwrap();
    let mut replacement = selected.resolved.clone().unwrap();
    replacement.symbol_id = wrong.id;
    store
        .batch_update_resolutions(&[(selected.id, replacement.clone())])
        .unwrap();
    selected.resolved = Some(replacement);
    // A stale building graph contains a wrong primary edge. Selection must
    // replace it, not leave it beside the compiler edge.
    store.delete_all_edges().unwrap();
    GraphBuilder::new(store.clone()).build_all(
        &refs
            .into_iter()
            .filter_map(|r| r.resolved.clone().map(|t| (r, t)))
            .collect::<Vec<_>>(),
    );
    run(&store, root.path(), Some(input()));
    let edges = store.get_all_edges().unwrap();
    let selected_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.ref_id == Some(selected_id))
        .collect();
    assert_eq!(selected_edges.len(), 1);
    assert_eq!(selected_edges[0].provenance, Provenance::Compiler);
    assert_ne!(selected_edges[0].target, wrong.id);
    assert_eq!(calls(&store).len(), 2);
    assert!(gaps(&store).is_empty());
    let compiled = calls(&store);
    let repeat = run(&store, root.path(), Some(input()));
    assert_eq!(repeat.indexed, 0);
    assert_eq!(repeat.edges_built, 0);
    assert_eq!(compiled, calls(&store));
    run(&store, root.path(), None);
    assert_eq!(baseline, calls(&store));
    assert!(
        store
            .get_metadata(KEY_COMPILER_CALL_GAPS)
            .unwrap()
            .is_none()
    );
}

#[test]
fn conflicting_compiler_targets_cannot_fall_back_to_a_source_primary_target() {
    let (root, store) = setup();
    run(&store, root.path(), None);
    let baseline = calls(&store);
    let mut compiler = input();
    let mut conflict = compiler.observations[0].clone();
    let start = SOURCE.find("namespace Right").unwrap() + "namespace Right { int ".len();
    let target = CompilerDeclaration {
        name: "target".into(),
        kind: atlas_engine::SymbolKind::Function,
        qualified_name: "Right::target".into(),
        location: CompilerLocation {
            path: "sample.cpp".into(),
            start_byte: start as u32,
            end_byte: (start + 6) as u32,
        },
    };
    conflict.declaration = target.clone();
    conflict.definition = Some(target);
    compiler.observations.push(conflict);
    run(&store, root.path(), Some(compiler));
    let limits = gaps(&store);
    assert_eq!(limits.len(), 1);
    assert_eq!(
        limits[0].reason,
        CompilerBindingGapReason::ConflictingTargets
    );
    let id = limits[0].reference_id.unwrap();
    assert!(
        store
            .get_reference_by_id(id.as_bytes())
            .unwrap()
            .unwrap()
            .resolved
            .is_none()
    );
    let remaining = calls(&store);
    assert_eq!(remaining.len(), 1);
    assert!(
        remaining
            .iter()
            .all(|(id, edge)| baseline.get(id) == Some(edge))
    );
    // Changing compiler selection with unchanged source re-runs resolution.
    run(&store, root.path(), Some(input()));
    assert_eq!(calls(&store).len(), 2);
    assert!(gaps(&store).is_empty());
}

#[test]
fn changed_indexed_dependency_invalidates_compiler_facts_in_unchanged_callers() {
    let (root, store) = setup();
    let dep = "#define TARGET_MODE 1\n";
    std::fs::write(root.path().join("dependency.cpp"), dep).unwrap();
    let mut compiler = input();
    compiler.inputs.insert(
        "dependency.cpp".into(),
        blake3::hash(dep.as_bytes()).to_hex().to_string(),
    );
    run(&store, root.path(), Some(compiler.clone()));
    assert!(
        store
            .get_all_edges()
            .unwrap()
            .iter()
            .any(|e| e.provenance == Provenance::Compiler)
    );
    std::fs::write(
        root.path().join("dependency.cpp"),
        "#define TARGET_MODE 2\n",
    )
    .unwrap();
    run(&store, root.path(), Some(compiler));
    assert_eq!(calls(&store).len(), 2);
    assert!(
        store
            .get_all_edges()
            .unwrap()
            .iter()
            .all(|e| e.provenance != Provenance::Compiler)
    );
    let limits = gaps(&store);
    assert_eq!(
        limits[0].reason,
        CompilerBindingGapReason::IndexedInputMismatch
    );
    assert!(limits[0].reference_id.is_none());
    assert_eq!(limits[0].location.path, "sample.cpp");
    assert_eq!(limits[0].input_path.as_deref(), Some("dependency.cpp"));
}

#[test]
fn missing_compiler_owner_keeps_independent_source_facts_and_a_located_limit() {
    let (root, store) = setup();
    run(&store, root.path(), None);
    let baseline = calls(&store);
    let mut compiler = input();
    compiler.observations[0].owner = None;
    run(&store, root.path(), Some(compiler));
    assert_eq!(baseline, calls(&store));
    let limits = gaps(&store);
    assert_eq!(limits[0].reason, CompilerBindingGapReason::OwnerUnavailable);
    assert!(limits[0].reference_id.is_some());
}

#[test]
fn selected_header_declaration_preserves_the_independently_resolved_body() {
    let header = "struct Token {}; struct Factory { static int choose(Token); };\n";
    let body = "#include \"api.hpp\"\nint Factory::choose(Token value) { return 1; }\n";
    let caller =
        "#include \"api.hpp\"\nint selected(Token value) { return Factory::choose(value); }\n";
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let mut inputs = BTreeMap::new();
    for (path, text) in [
        ("api.hpp", header),
        ("body.cpp", body),
        ("sample.cpp", caller),
    ] {
        std::fs::write(root.path().join(path), text).unwrap();
        inputs.insert(
            path.into(),
            blake3::hash(text.as_bytes()).to_hex().to_string(),
        );
    }
    run(&store, root.path(), None);
    let baseline = calls(&store);
    assert_eq!(baseline.len(), 1);
    let edge = store
        .get_all_edges()
        .unwrap()
        .into_iter()
        .find(|e| e.kind.as_str() == "calls")
        .unwrap();
    let target = store.find_symbol_by_id(&edge.target).unwrap().unwrap();
    assert_eq!(
        store.get_file(&target.file_id).unwrap().unwrap().path,
        "body.cpp"
    );
    let mut selected = location(header, "choose");
    selected.path = "api.hpp".into();
    let mut call = location(caller, "choose(value)");
    call.end_byte = call.start_byte + 6;
    let compiler = CompilerCallInput {
        inputs,
        observations: vec![CompilerObservation {
            kind: CompilerObservationKind::CallDeclaration,
            location: call,
            call_expression: Some(location(caller, "Factory::choose(value)")),
            declaration: CompilerDeclaration {
                name: "choose".into(),
                kind: atlas_engine::SymbolKind::Method,
                qualified_name: "Factory::choose".into(),
                location: selected,
            },
            definition: None,
            owner: Some(CompilerDeclaration {
                name: "selected".into(),
                kind: atlas_engine::SymbolKind::Function,
                qualified_name: "selected".into(),
                location: location(caller, "selected"),
            }),
            dispatch: CompilerDispatch::Direct,
        }],
    };
    run(&store, root.path(), Some(compiler.clone()));
    assert_eq!(baseline, calls(&store));
    let reference = store
        .get_reference_by_id(edge.ref_id.unwrap().as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(reference.resolved.as_ref().unwrap().symbol_id, target.id);
    assert_eq!(
        reference.resolved.as_ref().unwrap().provenance,
        Provenance::TreeSitter
    );
    assert_eq!(
        gaps(&store)[0].reason,
        CompilerBindingGapReason::DefinitionUnavailable
    );
    let repeat = run(&store, root.path(), Some(compiler));
    assert_eq!(repeat.edges_built, 0);
    assert_eq!(baseline, calls(&store));
}

#[test]
fn a_compiler_selected_overload_does_not_inherit_another_overloads_body() {
    let text = "int target(int value) { return value; } int target(double); int selected() { return target(1); }\n";
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("sample.cpp"), text).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    run(&store, root.path(), None);
    let mut at = location(text, "target(1)");
    at.end_byte = at.start_byte + 6;
    let mut declaration = location(text, "target(double)");
    declaration.end_byte = declaration.start_byte + 6;
    let compiler = CompilerCallInput {
        inputs: BTreeMap::from([(
            "sample.cpp".into(),
            blake3::hash(text.as_bytes()).to_hex().to_string(),
        )]),
        observations: vec![CompilerObservation {
            kind: CompilerObservationKind::CallDeclaration,
            location: at,
            call_expression: Some(location(text, "target(1)")),
            declaration: CompilerDeclaration {
                name: "target".into(),
                kind: atlas_engine::SymbolKind::Function,
                qualified_name: "target".into(),
                location: declaration.clone(),
            },
            definition: None,
            owner: Some(CompilerDeclaration {
                name: "selected".into(),
                kind: atlas_engine::SymbolKind::Function,
                qualified_name: "selected".into(),
                location: location(text, "selected"),
            }),
            dispatch: CompilerDispatch::Direct,
        }],
    };
    let report = atlas_engine::associate_compiler_calls(
        &store,
        &compiler.observations,
        &compiler.inputs,
        root.path(),
        &mut || false,
    )
    .unwrap();
    let binding = &report.resolved[0];
    let wrong = store
        .get_all_symbols()
        .unwrap()
        .into_iter()
        .find(|s| s.name == "target" && s.range != s.name_range)
        .unwrap();
    // Simulate a weaker producer selecting the other overload. Name agreement
    // must not attach that body to an exact compiler-selected declaration.
    let mut target = binding.target.clone();
    target.symbol_id = wrong.id;
    target.strategy = ResolutionStrategy::ExactMatch;
    target.provenance = Provenance::TreeSitter;
    let mut source = vec![(binding.reference.clone(), target)];
    store.delete_all_edges().unwrap();
    resolution::compiler::apply_compiler_call_bindings(&store, &mut source, &report).unwrap();
    assert_eq!(source.len(), 1);
    assert_eq!(source[0].1.symbol_id, binding.target.symbol_id);
    assert_eq!(source[0].1.provenance, Provenance::Compiler);
    assert_ne!(source[0].1.symbol_id, wrong.id);
}

#[test]
fn compiler_declaration_uses_the_existing_unique_body_association_without_a_source_call() {
    let header = "template<class T> struct Handle {}; struct Api { struct Item {}; Handle<Item> read(); }; Api* acquire();\n";
    let body = "#include \"api.hpp\"\nHandle<Api::Item> Api::read() { return {}; }\n";
    let caller = "#include \"api.hpp\"\nauto selected() { return acquire()->read(); }\n";
    let (root, store, input) = declared_body_case(header, body, caller);
    run(&store, root.path(), None);
    let reference = store
        .get_all_call_references()
        .unwrap()
        .into_iter()
        .find(|r| r.file_id == types::FileId::generate("sample.cpp") && r.name == "read")
        .unwrap();
    assert!(reference.resolved.is_none(), "{reference:?}");
    let navigation = atlas_engine::cpp_declaration_navigation(&store, &|| false).unwrap();
    let member = navigation
        .iter()
        .find(|r| r.declaration.qualified_name == "Api")
        .unwrap()
        .members
        .iter()
        .find(|m| m.declaration.name == "read")
        .unwrap();
    assert_eq!(member.definitions.len(), 1);
    let expected = member.definitions[0].id;
    run(&store, root.path(), Some(input.clone()));
    let selected = store
        .get_reference_by_id(reference.id.as_bytes())
        .unwrap()
        .unwrap()
        .resolved
        .unwrap();
    assert_eq!(selected.symbol_id, expected);
    assert_eq!(selected.provenance, Provenance::Compiler);
    assert_eq!(selected.strategy, ResolutionStrategy::Compiler);
    assert!(
        gaps(&store)
            .iter()
            .any(|g| g.reason == CompilerBindingGapReason::DefinitionUnavailable)
    );
    let before = calls(&store);
    let repeat = run(&store, root.path(), Some(input));
    assert_eq!(repeat.edges_built, 0);
    assert_eq!(calls(&store), before);
}

fn declared_body_case(
    header: &str,
    body: &str,
    caller: &str,
) -> (tempfile::TempDir, Arc<Store>, CompilerCallInput) {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let mut inputs = BTreeMap::new();
    for (path, text) in [
        ("api.hpp", header),
        ("body.cpp", body),
        ("sample.cpp", caller),
    ] {
        std::fs::write(root.path().join(path), text).unwrap();
        inputs.insert(
            path.into(),
            blake3::hash(text.as_bytes()).to_hex().to_string(),
        );
    }
    let mut declaration = location(header, "read");
    declaration.path = "api.hpp".into();
    let mut at = location(caller, "read()");
    at.end_byte = at.start_byte + 4;
    let input = CompilerCallInput {
        inputs,
        observations: vec![CompilerObservation {
            kind: CompilerObservationKind::CallDeclaration,
            location: at,
            call_expression: Some(location(caller, "acquire()->read()")),
            declaration: CompilerDeclaration {
                name: "read".into(),
                kind: atlas_engine::SymbolKind::Method,
                qualified_name: "Api::read".into(),
                location: declaration,
            },
            definition: None,
            owner: Some(CompilerDeclaration {
                name: "selected".into(),
                kind: atlas_engine::SymbolKind::Function,
                qualified_name: "selected".into(),
                location: location(caller, "selected"),
            }),
            dispatch: CompilerDispatch::Direct,
        }],
    };
    (root, store, input)
}

#[test]
fn compiler_declaration_body_composition_keeps_ambiguity_and_unsupported_signatures() {
    let header = "struct Api { int read(); }; Api* acquire();\n";
    let caller = "#include \"api.hpp\"\nint selected() { return acquire()->read(); }\n";
    for body in [
        "#include \"api.hpp\"\nint Api::read(int value) { return value; }\n",
        "#include \"api.hpp\"\nint Api::read() const { return 1; }\n",
        "namespace other { struct Api { int read() { return 1; } }; }\n",
        "#include \"api.hpp\"\ntemplate<class T> int Api::read() { return 1; }\n",
    ] {
        let (root, store, input) = declared_body_case(header, body, caller);
        run(&store, root.path(), Some(input));
        let reference = store
            .get_all_call_references()
            .unwrap()
            .into_iter()
            .find(|r| r.file_id == types::FileId::generate("sample.cpp") && r.name == "read")
            .unwrap();
        let target = store
            .find_symbol_by_id(&reference.resolved.unwrap().symbol_id)
            .unwrap()
            .unwrap();
        assert_eq!(target.range, target.name_range, "{body}: {target:?}");
        assert_eq!(target.file_id, types::FileId::generate("api.hpp"));
    }
    let body = "#include \"api.hpp\"\nint Api::read() { return 1; }\n";
    let (root, store, input) = declared_body_case(header, body, caller);
    std::fs::write(root.path().join("alternative.cpp"), body).unwrap();
    run(&store, root.path(), Some(input));
    let reference = store
        .get_all_call_references()
        .unwrap()
        .into_iter()
        .find(|r| r.file_id == types::FileId::generate("sample.cpp") && r.name == "read")
        .unwrap();
    let target = store
        .find_symbol_by_id(&reference.resolved.unwrap().symbol_id)
        .unwrap()
        .unwrap();
    assert_eq!(target.range, target.name_range, "{target:?}");
}

struct StopAtResolution(AtomicBool);
impl ProgressSink for StopAtResolution {
    fn emit(&self, event: ProgressEvent) {
        if matches!(
            event,
            ProgressEvent::PhaseStarted {
                phase: PhaseName::Resolution,
                ..
            }
        ) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

#[test]
fn canceled_compiler_association_is_not_finalized_and_can_be_retried() {
    let (root, store) = setup();
    let stop = StopAtResolution(AtomicBool::new(false));
    let result = pipeline(&store, root.path(), Some(input()))
        .run(&stop, &mut || stop.0.load(Ordering::Relaxed));
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(store.get_metadata("last_index_time").unwrap().is_none());
    run(&store, root.path(), Some(input()));
    assert_eq!(calls(&store).len(), 2);
    assert!(store.get_metadata("last_index_time").unwrap().is_some());
    let before = calls(&store);
    // Incremental sync has no compiler input selection. It must not silently
    // retain stale compiler results or discard their provenance on edits.
    let result = filesync::IncrementalPipeline::new(
        store.clone(),
        root.path().into(),
        ExtractionMode::Structural,
    )
    .sync(&NoopSink, &mut || false);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("compiler-enriched")
    );
    assert_eq!(before, calls(&store));
    assert!(store.get_all_call_references().unwrap().iter().any(|r| {
        r.resolved
            .as_ref()
            .is_some_and(|t| t.strategy == ResolutionStrategy::Compiler)
    }));
}

#[test]
fn compiler_only_declarations_have_bounded_identity_and_are_removed_with_the_input() {
    let text = "#define FUNCTION(name) int name() { return 1; }\nnamespace Api { FUNCTION(make) }\nint entry() { return Api::make(); } int neighbor() { return Api::make(); }\nint local() { return 2; } int ordinary() { return local(); }\n";
    let (root, store) = setup();
    std::fs::write(root.path().join("sample.cpp"), text).unwrap();
    run(&store, root.path(), None);
    let baseline = calls(&store);
    let source_symbols = store.get_all_symbols().unwrap();
    let mut at = location(text, "FUNCTION(make)");
    at.start_byte += 9;
    at.end_byte -= 1;
    assert!(
        !source_symbols
            .iter()
            .any(|symbol| symbol.name_range.start_byte == at.start_byte)
    );
    let declared = CompilerDeclaration {
        name: "make".into(),
        kind: atlas_engine::SymbolKind::Function,
        qualified_name: "Api::make".into(),
        location: at,
    };
    let mut occurrence = location(text, "Api::make()");
    occurrence.start_byte += 5;
    occurrence.end_byte -= 2;
    let compiler = CompilerCallInput {
        inputs: BTreeMap::from([(
            "sample.cpp".into(),
            blake3::hash(text.as_bytes()).to_hex().to_string(),
        )]),
        observations: vec![CompilerObservation {
            kind: CompilerObservationKind::CallDeclaration,
            location: occurrence,
            call_expression: Some(location(text, "Api::make()")),
            declaration: declared.clone(),
            definition: Some(declared),
            owner: Some(CompilerDeclaration {
                name: "entry".into(),
                kind: atlas_engine::SymbolKind::Function,
                qualified_name: "entry".into(),
                location: location(text, "entry"),
            }),
            dispatch: CompilerDispatch::Direct,
        }],
    };
    run(&store, root.path(), Some(compiler.clone()));
    let admitted: Vec<_> = store
        .get_all_symbols()
        .unwrap()
        .into_iter()
        .filter(|symbol| symbol.layer == atlas_engine::layer::COMPILER_DECLARATION)
        .collect();
    assert_eq!(admitted.len(), 1);
    let endpoint = &admitted[0];
    assert_eq!(endpoint.qualified_name, "Api::make");
    assert_eq!(endpoint.range, endpoint.name_range);
    assert!(endpoint.signature.is_none());
    let edges = store.get_all_edges().unwrap();
    let selected: Vec<_> = edges
        .iter()
        .filter(|edge| edge.target == endpoint.id)
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "an unobserved neighboring call must remain unresolved"
    );
    assert_eq!(selected[0].provenance, Provenance::Compiler);
    assert!(
        gaps(&store)
            .iter()
            .any(|gap| gap.reason == CompilerBindingGapReason::DefinitionNotIndexed)
    );
    let stable = calls(&store);
    let repeated = run(&store, root.path(), Some(compiler.clone()));
    assert_eq!(repeated.indexed, 0);
    assert_eq!(repeated.edges_built, 0);
    assert_eq!(stable, calls(&store));
    // Conflicting declaration metadata at the same written invocation admits neither endpoint.
    let mut conflict = compiler.clone();
    let mut alternative = compiler.observations[0].clone();
    alternative.declaration.qualified_name = "Other::make".into();
    alternative.definition = Some(alternative.declaration.clone());
    conflict.observations.push(alternative);
    run(&store, root.path(), Some(conflict));
    assert_eq!(baseline, calls(&store));
    assert!(
        store
            .get_all_symbols()
            .unwrap()
            .iter()
            .all(|symbol| symbol.layer != atlas_engine::layer::COMPILER_DECLARATION)
    );
    run(&store, root.path(), Some(compiler));
    assert_eq!(stable, calls(&store));
    run(&store, root.path(), None);
    assert_eq!(baseline, calls(&store));
    let mut after = store.get_all_symbols().unwrap();
    let mut before = source_symbols;
    after.sort_by_key(|symbol| symbol.id);
    before.sort_by_key(|symbol| symbol.id);
    assert_eq!(before, after);
}
