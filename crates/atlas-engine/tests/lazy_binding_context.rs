#![cfg(all(feature = "cpp", feature = "typescript"))]

use std::{collections::BTreeMap, path::Path};

use atlas_engine::{
    DataNodeKind, Engine, ExtractionMode, FileId, Language, create_frontend, extract_file_with_mode,
};
use types::lazy::AnalysisUnit;

#[test]
fn lazy_field_receivers_match_full_after_rebinding_and_nested_access() {
    let source = "struct Box { int slot; }; struct Wrapper { Box child; }; Wrapper choose(Wrapper input) { return input; } int selected(Wrapper object, Wrapper replacement) { object = replacement; return choose(object).child.slot; }";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("receiver.cpp");
    std::fs::write(&path, source).unwrap();
    let file = FileId::generate("receiver.cpp");
    let frontend = create_frontend(Language::Cpp).unwrap();
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let full = extract_file_with_mode(
        &frontend,
        file,
        &path,
        source,
        &hash,
        ExtractionMode::Full,
        &(),
    )
    .unwrap();
    let structural = extract_file_with_mode(
        &frontend,
        file,
        &path,
        source,
        &hash,
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    let function = full.symbols.iter().find(|s| s.name == "selected").unwrap();
    let engine = Engine::open(&dir.path().join("atlas.db")).unwrap();
    engine.store().init_schema().unwrap();
    engine.insert_facts(&structural).unwrap();
    let expected_nodes: BTreeMap<_, _> = full
        .data_nodes
        .iter()
        .filter(|n| n.function_id == Some(function.id))
        .map(|n| (n.id, n))
        .collect();
    let expected_edges: BTreeMap<_, _> = full
        .dataflow_edges
        .iter()
        .filter(|e| expected_nodes.contains_key(&e.source))
        .map(|e| (e.id, e))
        .collect();
    for cold in [true, false] {
        let window = engine
            .materialize()
            .dataflow()
            .ensure_for_function_with_depth(&function.id, 0, None)
            .unwrap();
        assert_eq!(window.units_built, usize::from(cold));
        assert_eq!(window.units_cached, usize::from(!cold));
        let nodes = engine
            .store()
            .find_data_nodes_by_function(&function.id)
            .unwrap();
        let edges = engine
            .store()
            .find_dataflow_edges_by_function(&function.id)
            .unwrap();
        assert_eq!(
            nodes.iter().map(|n| (n.id, n)).collect::<BTreeMap<_, _>>(),
            expected_nodes
        );
        assert_eq!(
            edges.iter().map(|e| (e.id, e)).collect::<BTreeMap<_, _>>(),
            expected_edges
        );
    }
}

#[test]
fn lazy_enclosing_bindings_survive_build_order_and_recomputation() {
    for (language, name, source) in [
        (
            Language::Cpp,
            "context.cpp",
            "int selected(int input) { auto inner = [&input]() { { int input=7; input++; } return input; }; return 0; } int unrelated(int input) { return input; }",
        ),
        (
            Language::TypeScript,
            "context.ts",
            "function selected(input: number) { function inner() { { let input=7; input++; } return input; } return 0; } function unrelated(input: number) { return input; }",
        ),
        (
            Language::Cpp,
            "initializers.cpp",
            "int selected(int input) { auto inner = [saved = input + 1] { return saved; }; return 0; } int unrelated(int input) { return input; }",
        ),
    ] {
        for inner_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);
            std::fs::write(&path, source).unwrap();
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let file = FileId::generate(name);
            let frontend = create_frontend(language).unwrap();
            let extract = |mode| {
                extract_file_with_mode(&frontend, file, &path, source, &hash, mode, &()).unwrap()
            };
            let full = extract(ExtractionMode::Full);
            let structural = extract(ExtractionMode::Structural);
            let engine = Engine::open(&dir.path().join("atlas.db")).unwrap();
            engine.store().init_schema().unwrap();
            engine.insert_facts(&structural).unwrap();
            let outer = full.symbols.iter().find(|s| s.name == "selected").unwrap();
            let inner = full
                .symbols
                .iter()
                .find(|s| {
                    s.name == "inner" && s.kind == types::SymbolKind::Function
                        || s.name.starts_with("<lambda@")
                })
                .unwrap();
            let other = full.symbols.iter().find(|s| s.name == "unrelated").unwrap();
            let declared: BTreeMap<_, _> = structural.bindings.iter().map(|b| (b.id, b)).collect();
            let check = |function: &types::SymbolDef| {
                let actual = engine
                    .store()
                    .find_data_nodes_by_function(&function.id)
                    .unwrap();
                let actual: BTreeMap<_, _> = actual.iter().map(|n| (n.id, n)).collect();
                let expected: BTreeMap<_, _> = full
                    .data_nodes
                    .iter()
                    .filter(|n| n.function_id == Some(function.id))
                    .map(|n| (n.id, n))
                    .collect();
                assert_eq!(
                    actual, expected,
                    "{language:?}, inner_first={inner_first}, {}",
                    function.name
                );
                let uses = engine.store().find_binding_uses_by_file(&file).unwrap();
                let inside = |range: &types::TextRange| {
                    range.start_byte >= function.range.start_byte
                        && range.end_byte <= function.range.end_byte
                };
                let actual: BTreeMap<_, _> = uses
                    .iter()
                    .filter(|u| inside(&u.range))
                    .map(|u| (u.id, u))
                    .collect();
                let expected: BTreeMap<_, _> = full
                    .binding_uses
                    .iter()
                    .filter(|u| inside(&u.range))
                    .map(|u| (u.id, u))
                    .collect();
                assert_eq!(
                    actual, expected,
                    "binding uses retain their own source scope"
                );
                let bindings = engine.store().find_bindings_by_file(&file).unwrap();
                assert_eq!(declared, bindings.iter().map(|b| (b.id, b)).collect());
                assert!(
                    engine
                        .store()
                        .find_data_nodes_by_function(&other.id)
                        .unwrap()
                        .is_empty()
                );
            };
            let order = if inner_first {
                [inner, outer]
            } else {
                [outer, inner]
            };
            for (index, function) in order.iter().enumerate() {
                let window = engine
                    .materialize()
                    .dataflow()
                    .ensure_for_function_with_depth(&function.id, 0, None)
                    .unwrap();
                assert_eq!(window.units_built, 1);
                for loaded in &order[..=index] {
                    check(loaded);
                }
            }
            // Rebuild only the outer unit, while the inner one remains cached.
            let unit = AnalysisUnit::from_function(file, outer.id, outer.range);
            engine.store().with_transaction(|tx| {
                tx.execute("UPDATE extraction_state SET dataflow_version=0 WHERE file_id=?1 AND unit_id=?2 AND layer='dataflow'", (file, &unit.unit_id[..]))?;
                Ok(())
            }).unwrap();
            let rebuilt = engine
                .materialize()
                .dataflow()
                .ensure_for_function_with_depth(&outer.id, 0, None)
                .unwrap();
            assert_eq!(rebuilt.units_built, 1);
            check(outer);
            check(inner);
            let cached = engine
                .materialize()
                .dataflow()
                .ensure_for_function_with_depth(&inner.id, 0, None)
                .unwrap();
            assert_eq!(cached.units_cached, 1);
            check(inner);
            // Lexical observations are removed by source replacement.
            std::fs::write(&path, "").unwrap();
            let empty = extract_file_with_mode(
                &frontend,
                file,
                &path,
                "",
                blake3::hash(b"").to_hex().as_ref(),
                ExtractionMode::Structural,
                &(),
            )
            .unwrap();
            engine
                .store()
                .replace_file_facts_with_invalidation(&file, &empty)
                .unwrap();
            assert!(
                engine
                    .store()
                    .find_bindings_by_file(&file)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                engine
                    .store()
                    .find_binding_uses_by_file(&file)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                engine
                    .store()
                    .find_data_nodes_by_file(&file)
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[test]
fn unit_data_nodes_reuse_existing_bindings_only_from_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(&dir.path().join("atlas.db")).unwrap();
    engine.store().init_schema().unwrap();
    let source = "int selected(int input) { return input; }";
    let mut all = Vec::new();
    for name in ["first.cpp", "second.cpp"] {
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate(name),
            Path::new(name),
            source,
            "fixed",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        engine.insert_facts(&facts).unwrap();
        all.push(facts);
    }
    let facts = &all[0];
    let function = facts.symbols.iter().find(|s| s.name == "selected").unwrap();
    let binding = facts.bindings.iter().find(|b| b.name == "input").unwrap();
    let other_binding = all[1].bindings.iter().find(|b| b.name == "input").unwrap();
    let parameter = facts
        .data_nodes
        .iter()
        .find(|n| n.kind == DataNodeKind::Parameter)
        .unwrap();
    let unit = AnalysisUnit::from_function(facts.file.file_id, function.id, function.range);
    for (binding_id, expected) in [
        (binding.id, Some(binding.id)),
        (other_binding.id, None),
        (
            types::BindingId::generate(
                &facts.file.file_id,
                &binding.scope_id,
                "local",
                "absent",
                200,
            ),
            None,
        ),
    ] {
        let mut node = parameter.clone();
        node.binding_id = Some(binding_id);
        engine
            .store()
            .replace_dataflow_for_unit(&unit, &[node], &[], &[], &[], &[], &[])
            .unwrap();
        assert_eq!(
            engine
                .store()
                .get_data_node(&parameter.id)
                .unwrap()
                .unwrap()
                .binding_id,
            expected
        );
        assert!(
            engine
                .store()
                .find_bindings_by_file(&facts.file.file_id)
                .unwrap()
                .iter()
                .any(|b| b == binding),
            "empty dataflow payload must not erase a structural declaration"
        );
    }
}
