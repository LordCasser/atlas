#![cfg(all(feature = "cpp", feature = "typescript"))]

use std::{collections::BTreeMap, path::Path};

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::{
    DataNodeKind, FileFacts, FileId, Language,
    lazy::{AnalysisUnit, LazyWindow},
};

fn extract_pair(
    language: Language,
    source: &str,
    with_recorded_calls: bool,
) -> (FileFacts, FileFacts) {
    let path = "result-boundaries";
    let file = FileId::generate(path);
    let frontend = create_frontend(language).unwrap();
    let extract = |mode| {
        extract_file_with_mode(&frontend, file, Path::new(path), source, "fixed", mode, &())
            .unwrap()
    };
    let structural = extract(ExtractionMode::Structural);
    let full = extract(ExtractionMode::Full);
    let function = structural
        .symbols
        .iter()
        .find(|symbol| symbol.name == "selected")
        .unwrap();
    let unit = AnalysisUnit::from_function(file, function.id, function.range);
    let lazy = extract(ExtractionMode::LazyDataflow {
        include_parameter_outputs: true,
        window: LazyWindow {
            seed_unit: unit.clone(),
            units: vec![unit],
            variable_focus: None,
            truncated: false,
            units_built: 0,
            units_cached: 0,
            units_pending: 0,
            pending_job_ids: vec![],
            quality: None,
            capability_mask: Default::default(),
        },
        callsites: if with_recorded_calls {
            structural.callsites
        } else {
            vec![]
        },
    });
    assert!(!lazy.budget_exceeded);
    assert!(lazy.symbols.is_empty() && lazy.references.is_empty() && lazy.callsites.is_empty());
    assert!(
        lazy.data_nodes
            .iter()
            .all(|node| node.function_id == Some(function.id))
    );
    if with_recorded_calls {
        let full_nodes: BTreeMap<_, _> = full
            .data_nodes
            .iter()
            .filter(|node| node.function_id == Some(function.id))
            .map(|node| (node.id, node))
            .collect();
        let lazy_nodes: BTreeMap<_, _> =
            lazy.data_nodes.iter().map(|node| (node.id, node)).collect();
        assert_eq!(full_nodes, lazy_nodes, "{language:?}: {source}");
        let full_edges: BTreeMap<_, _> = full
            .dataflow_edges
            .iter()
            .filter(|edge| {
                full_nodes.contains_key(&edge.source) || full_nodes.contains_key(&edge.target)
            })
            .map(|edge| (edge.id, edge))
            .collect();
        let lazy_edges: BTreeMap<_, _> = lazy
            .dataflow_edges
            .iter()
            .map(|edge| (edge.id, edge))
            .collect();
        assert_eq!(full_edges, lazy_edges, "{language:?}: {source}");
    }
    (full, lazy)
}

#[test]
fn construction_boundaries_match_full_and_lazy_without_a_return_mapping() {
    for (language, source) in [
        (
            Language::Cpp,
            "struct Item { Item(int) {} }; int read(int x) { return x; } void consume(Item*); void selected(int input) { consume(new Item(read(input))); } Item* other(int x) { return new Item(x); }",
        ),
        (
            Language::TypeScript,
            "class Item { constructor(x: number) {} } function read(x: number) { return x; } function consume(x: Item) {} function selected(input: number) { consume(new Item(read(input))); } function other(x: number) { return new Item(x); }",
        ),
    ] {
        let (_, lazy) = extract_pair(language, source, true);
        let results: Vec<_> = lazy
            .data_nodes
            .iter()
            .filter(|node| node.kind == DataNodeKind::CallReturn)
            .collect();
        assert_eq!(results.len(), 3, "{language:?}");
        let construction = results
            .iter()
            .find(|node| node.name.as_deref() == Some("new Item(read(input))"))
            .unwrap();
        assert!(construction.callsite_id.is_none());
        let nodes: BTreeMap<_, _> = lazy.data_nodes.iter().map(|node| (node.id, node)).collect();
        let outer_argument = lazy
            .data_nodes
            .iter()
            .find(|node| node.kind == DataNodeKind::CallArg && node.range == construction.range)
            .unwrap();
        let incoming: Vec<_> = lazy
            .dataflow_edges
            .iter()
            .filter(|edge| edge.target == outer_argument.id)
            .map(|edge| nodes[&edge.source])
            .collect();
        assert_eq!(
            incoming,
            vec![*construction],
            "nested call and constructor operands cannot supply the outer argument value"
        );
    }
}

#[test]
fn missing_call_records_keep_source_boundaries_without_inventing_call_identity() {
    for (language, source) in [
        (
            Language::Cpp,
            "int selected(int input) { return external(input); }",
        ),
        (
            Language::TypeScript,
            "function selected(input: number) { return external(input); }",
        ),
    ] {
        let (_, lazy) = extract_pair(language, source, false);
        let result = lazy
            .data_nodes
            .iter()
            .find(|node| node.kind == DataNodeKind::CallReturn)
            .unwrap();
        assert_eq!(result.name.as_deref(), Some("external(input)"));
        assert!(result.callsite_id.is_none());
        let returned = lazy
            .data_nodes
            .iter()
            .find(|node| node.kind == DataNodeKind::Return)
            .unwrap();
        let incoming: Vec<_> = lazy
            .dataflow_edges
            .iter()
            .filter(|edge| edge.target == returned.id)
            .map(|edge| edge.source)
            .collect();
        assert_eq!(incoming, vec![result.id]);
    }
}

#[test]
fn lazy_call_results_use_structural_identity_and_match_full_value_edges() {
    for (language, path, source) in [
        (
            Language::Cpp,
            "calls.cpp",
            "int choose(int ignored, int input) { return input; } int selected(int first, int second) { return choose(second, first + 3); } int other(int input) { return choose(input, input); }",
        ),
        (
            Language::TypeScript,
            "calls.ts",
            "function choose(ignored: number, input: number) { return input; } function selected(first: number, second: number) { return choose(second, first + 3); } function other(input: number) { return choose(input, input); }",
        ),
    ] {
        let frontend = create_frontend(language).unwrap();
        let file = FileId::generate(path);
        let extract = |mode| {
            extract_file_with_mode(&frontend, file, Path::new(path), source, "fixed", mode, &())
                .unwrap()
        };
        let structural = extract(ExtractionMode::Structural);
        let full = extract(ExtractionMode::Full);
        let function = structural
            .symbols
            .iter()
            .find(|symbol| symbol.name == "selected")
            .unwrap();
        let unit = AnalysisUnit::from_function(file, function.id, function.range);
        let window = LazyWindow {
            seed_unit: unit.clone(),
            units: vec![unit],
            variable_focus: None,
            truncated: false,
            units_built: 0,
            units_cached: 0,
            units_pending: 0,
            pending_job_ids: vec![],
            quality: None,
            capability_mask: Default::default(),
        };
        let lazy = extract(ExtractionMode::LazyDataflow {
            include_parameter_outputs: true,
            window,
            callsites: structural.callsites.clone(),
        });
        assert!(!lazy.budget_exceeded);
        assert!(lazy.symbols.is_empty() && lazy.references.is_empty() && lazy.callsites.is_empty());
        assert!(
            lazy.data_nodes
                .iter()
                .all(|node| node.function_id == Some(function.id))
        );
        let full_nodes: BTreeMap<_, _> = full
            .data_nodes
            .iter()
            .filter(|node| node.function_id == Some(function.id))
            .map(|node| (node.id, node))
            .collect();
        let lazy_nodes: BTreeMap<_, _> =
            lazy.data_nodes.iter().map(|node| (node.id, node)).collect();
        assert_eq!(
            full_nodes, lazy_nodes,
            "{language:?}: exact invocation/result identity"
        );
        let results: Vec<_> = lazy
            .data_nodes
            .iter()
            .filter(|node| node.kind == DataNodeKind::CallReturn)
            .collect();
        assert_eq!(results.len(), 1);
        let call = structural
            .callsites
            .iter()
            .find(|call| call.caller == function.id)
            .unwrap();
        assert_eq!(results[0].callsite_id, Some(call.id));
        assert_eq!(results[0].range, call.range);
        let full_edges: BTreeMap<_, _> = full
            .dataflow_edges
            .iter()
            .filter(|edge| {
                full_nodes.contains_key(&edge.source) || full_nodes.contains_key(&edge.target)
            })
            .map(|edge| (edge.id, edge))
            .collect();
        let lazy_edges: BTreeMap<_, _> = lazy
            .dataflow_edges
            .iter()
            .map(|edge| (edge.id, edge))
            .collect();
        assert_eq!(
            full_edges, lazy_edges,
            "{language:?}: same value/evaluation boundary"
        );
    }
}
