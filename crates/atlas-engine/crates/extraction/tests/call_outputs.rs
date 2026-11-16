#![cfg(feature = "cpp")]
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use std::path::Path;
use types::{
    DataNodeKind, FileId, Language,
    lazy::{AnalysisUnit, LazyWindow},
};

#[test]
fn full_and_lazy_preserve_output_identity_separately_from_the_call_input() {
    let source = "void update(int&); int f(int first) { int value = first; update(value); return value; } int other(int input) { return input; }\n";
    let file = FileId::generate("flow.cpp");
    let extract = |mode| {
        extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            file,
            Path::new("flow.cpp"),
            source,
            "test",
            mode,
            &(),
        )
        .unwrap()
    };
    let full = extract(ExtractionMode::Full);
    let function = full.symbols.iter().find(|s| s.name == "f").unwrap();
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
        callsites: full.callsites.clone(),
    });
    let outputs = |facts: &types::FileFacts| {
        facts
            .data_nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::CallOutput)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(outputs(&full), outputs(&lazy));
    assert_eq!(outputs(&full).len(), 1);
    let output = outputs(&full).remove(0);
    let call = full
        .callsites
        .iter()
        .find(|c| Some(c.id) == output.callsite_id)
        .unwrap();
    assert_eq!(output.arg_index, Some(0));
    let arg = full
        .data_nodes
        .iter()
        .find(|n| Some(n.id) == call.args[0].data_node_id)
        .unwrap();
    assert_eq!(arg.kind, DataNodeKind::CallArg);
    for facts in [&full, &lazy] {
        assert!(facts.dataflow_edges.iter().any(|e| e.source == output.id));
        assert!(
            !facts
                .dataflow_edges
                .iter()
                .any(|e| e.source == output.id && e.target == arg.id)
        );
        assert!(!facts.budget_exceeded);
    }
    assert!(
        lazy.data_nodes
            .iter()
            .all(|n| n.function_id == Some(function.id))
    );
}

#[test]
fn unused_call_effects_do_not_displace_later_call_results_in_a_large_function() {
    let mut source = String::from("int produce(int input) { return input; } int run() {\n");
    for i in 0..230 {
        source.push_str(&format!("int value{i} = 0; observe(value{i});\n"));
    }
    source.push_str("int result = produce(7); return result; }\n");
    let file = FileId::generate("flow.cpp");
    let extract = |mode| {
        extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            file,
            Path::new("flow.cpp"),
            &source,
            "test",
            mode,
            &(),
        )
        .unwrap()
    };
    let full = extract(ExtractionMode::Full);
    let owner = full.symbols.iter().find(|s| s.name == "run").unwrap();
    let unit = AnalysisUnit::from_function(file, owner.id, owner.range);
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
        callsites: full.callsites.clone(),
    });
    assert!(!lazy.budget_exceeded);
    let call = lazy
        .data_nodes
        .iter()
        .find(|n| n.kind == DataNodeKind::CallReturn && n.name.as_deref() == Some("produce(7)"))
        .expect("last call result stays available");
    assert!(lazy.dataflow_edges.iter().any(|e| e.source == call.id));
    assert!(
        !full
            .data_nodes
            .iter()
            .any(|n| n.kind == DataNodeKind::CallOutput)
    );
    let expected = full
        .data_nodes
        .iter()
        .filter(|n| n.function_id == Some(owner.id))
        .collect::<Vec<_>>();
    assert_eq!(expected, lazy.data_nodes.iter().collect::<Vec<_>>());
}
