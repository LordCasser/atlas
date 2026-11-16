#![cfg(feature = "cpp")]

use std::{collections::BTreeMap, path::Path};

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::{
    DataNodeKind, FileFacts, FileId, Language,
    lazy::{AnalysisUnit, LazyWindow},
};

fn check_parameters(source: &str, function_name: &str, expected: &[(&str, u32)]) -> FileFacts {
    check_parameter_slots(
        source,
        function_name,
        &expected
            .iter()
            .map(|(name, index)| (*name, Some(*index)))
            .collect::<Vec<_>>(),
        true,
    )
}

fn check_parameter_slots(
    source: &str,
    function_name: &str,
    expected: &[(&str, Option<u32>)],
    clean: bool,
) -> FileFacts {
    let frontend = create_frontend(Language::Cpp).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&frontend.parser.tree_sitter_language())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !clean || !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let file = FileId::generate("parameters.cpp");
    let extract = |mode| {
        extract_file_with_mode(
            &frontend,
            file,
            Path::new("parameters.cpp"),
            source,
            "fixed",
            mode,
            &(),
        )
        .unwrap()
    };
    let structural = extract(ExtractionMode::Structural);
    let full = extract(ExtractionMode::Full);
    let function = structural
        .symbols
        .iter()
        .find(|s| s.name == function_name)
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
        callsites: structural.callsites,
    });
    for facts in [&full, &lazy] {
        let mut parameters: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|n| n.function_id == Some(function.id) && n.kind == DataNodeKind::Parameter)
            .collect();
        parameters.sort_by_key(|n| n.range.start_byte);
        assert_eq!(
            parameters
                .iter()
                .map(|n| (n.name.as_deref().unwrap(), n.arg_index))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(name, index)| (*name, *index))
                .collect::<Vec<_>>(),
            "{function_name}"
        );
        for parameter in parameters {
            assert!(parameter.binding_id.is_some(), "{parameter:?}");
            assert!(
                !facts
                    .data_nodes
                    .iter()
                    .any(|n| n.kind == DataNodeKind::Local && n.range == parameter.range),
                "a parameter declaration is not another local definition: {parameter:?}"
            );
        }
    }
    let full_nodes: BTreeMap<_, _> = full
        .data_nodes
        .iter()
        .filter(|n| {
            n.range.start_byte >= function.range.start_byte
                && n.range.end_byte <= function.range.end_byte
        })
        .map(|n| (n.id, n))
        .collect();
    let lazy_nodes: BTreeMap<_, _> = lazy.data_nodes.iter().map(|n| (n.id, n)).collect();
    assert_eq!(full_nodes, lazy_nodes);
    let full_edges: BTreeMap<_, _> = full
        .dataflow_edges
        .iter()
        .filter(|e| full_nodes.contains_key(&e.source) || full_nodes.contains_key(&e.target))
        .map(|e| (e.id, e))
        .collect();
    let lazy_edges: BTreeMap<_, _> = lazy.dataflow_edges.iter().map(|e| (e.id, e)).collect();
    assert_eq!(full_edges, lazy_edges);
    full
}

#[test]
fn declarators_preserve_parameter_identity_and_unnamed_argument_slots() {
    check_parameters(
        "int selected(int, int value, int* pointer, const int& reference, int values[3], int (*callback)(int hidden), int fallback = 9) { return fallback; }",
        "selected",
        &[
            ("value", 1),
            ("pointer", 2),
            ("reference", 3),
            ("values", 4),
            ("callback", 5),
            ("fallback", 6),
        ],
    );
}

#[test]
fn parameter_identity_survives_unknown_invocation_slots() {
    let partial = check_parameter_slots(
        "struct Settings {}; int selected(int input, const Settings& settings = {}) { return input; }",
        "selected",
        &[("input", None), ("settings", None)],
        false,
    );
    assert!(
        !partial.diagnostics.is_empty(),
        "parser limits must remain visible"
    );
    // A trailing parameter after an unexpanded pack has a declaration identity,
    // but its argument slot cannot be assigned from its written position.
    check_parameter_slots(
        "template<class... Items> int selected(Items... items, int input) { return input; }",
        "selected",
        &[("input", None)],
        true,
    );
}

#[test]
fn template_parameter_identity_does_not_require_type_substitution() {
    check_parameters(
        "template<class Work> int selected(Work&& task, const int& tag = 9) { return tag; }",
        "selected",
        &[("task", 0), ("tag", 1)],
    );
}

#[test]
fn nested_declarations_and_lambda_parameters_do_not_become_outer_arguments() {
    let facts = check_parameters(
        "int selected(int outer) { int prototype(int prototype_input); int (*callback)(int callback_input); auto task = [](const int& nested) { return nested; }; return outer; }",
        "selected",
        &[("outer", 0)],
    );
    let nested: Vec<_> = facts
        .data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("nested"))
        .collect();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].arg_index, Some(0));
    assert!(
        facts
            .symbols
            .iter()
            .any(|s| Some(s.id) == nested[0].function_id && s.name.starts_with("<lambda@"))
    );
    assert!(
        !facts
            .data_nodes
            .iter()
            .any(|n| n.kind == DataNodeKind::Parameter
                && matches!(
                    n.name.as_deref(),
                    Some("prototype_input" | "callback_input")
                ))
    );
}
