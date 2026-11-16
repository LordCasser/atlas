#![cfg(feature = "cpp")]

use std::{collections::HashSet, path::Path, sync::Arc};

use atlas_engine::{
    CallsiteId, DataNode, DataNodeId, DataNodeKind, ExtractionMode, FileId, Language,
    RawTraceEngine, ReferenceResolver, Store, TracePath, create_frontend, extract_file_with_mode,
};

struct Case {
    store: Arc<Store>,
    engine: RawTraceEngine,
}

impl Case {
    fn new(source: &str) -> Self {
        let file = FileId::generate("context.cpp");
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            file,
            Path::new("context.cpp"),
            source,
            "context-test",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&facts).unwrap();
        ReferenceResolver::new(store.clone())
            .resolve_for_files(&[file])
            .unwrap();
        Self {
            engine: RawTraceEngine::new(store.clone()),
            store,
        }
    }

    fn node(&self, function: &str, kind: DataNodeKind) -> DataNode {
        let functions = self.store.find_symbols_by_qname(function).unwrap();
        assert_eq!(functions.len(), 1);
        self.store
            .find_data_nodes_by_function(&functions[0].id)
            .unwrap()
            .into_iter()
            .filter(|n| n.kind == kind)
            .max_by_key(|n| n.range.start_byte)
            .unwrap()
    }

    fn trace(&self, function: &str, depth: usize) -> TracePath {
        let node = self.node(function, DataNodeKind::Return);
        let response = self.engine.trace_data_node(&node.id, depth, &[]);
        assert!(response.ok);
        let path = response.result.unwrap();
        assert_eq!(response.partial_result, path.partial_result);
        assert_chain(&path);
        path
    }
}

fn assert_chain(path: &TracePath) {
    let mut id = path.source.data_node.as_ref().unwrap().id;
    let mut context = &path.source.call_context;
    for (index, step) in path.steps.iter().enumerate() {
        assert_eq!(step.index as usize, index);
        assert_eq!(step.from_node_id, id);
        assert_eq!(&step.call_context, context);
        id = step.to_node_id;
        context = path
            .steps
            .get(index + 1)
            .map_or(&path.sink.call_context, |s| &s.call_context);
    }
    assert_eq!(id, path.sink.data_node.as_ref().unwrap().id);
}

const TWO_CALLERS: &str = r#"
int identity(int input) { return input; }
int selected(int first) { int value = identity(first); return value; }
int unrelated(int second) { int value = identity(second); return value; }
"#;

#[test]
fn field_receiver_projection_follows_the_use_after_rebinding_or_shadowing() {
    for source in [
        "struct Box { int slot; }; int selected(Box* object, Box* replacement) { object = replacement; return object->slot; }",
        "struct Box { int slot; }; int selected(Box* object, Box* replacement) { { auto object = replacement; return object->slot; } }",
    ] {
        let case = Case::new(source);
        let path = case.trace("selected", 60);
        assert_eq!(
            path.source.data_node.as_ref().unwrap().name.as_deref(),
            Some("replacement"),
            "{source}"
        );
        let step = path
            .steps
            .iter()
            .find(|s| s.edge_kind == atlas_engine::DataFlowKind::FieldLoad)
            .unwrap();
        let receiver = case
            .store
            .get_data_node(&step.from_node_id)
            .unwrap()
            .unwrap();
        assert_eq!(receiver.kind, DataNodeKind::Receiver);
        assert_eq!(
            receiver.range.start_byte as usize,
            source.rfind("object->slot").unwrap()
        );
        assert!(receiver.binding_id.is_some());
    }
}

#[test]
fn field_receiver_projection_preserves_call_results_and_each_nested_receiver() {
    let source = "struct Box { int slot; }; struct Wrapper { Box child; }; Wrapper choose(Wrapper input) { return input; } int selected(Wrapper object) { return choose(object).child.slot; }";
    let case = Case::new(source);
    let path = case.trace("selected", 60);
    assert_eq!(
        path.source.data_node.as_ref().unwrap().name.as_deref(),
        Some("object")
    );
    let receivers: HashSet<_> = path
        .steps
        .iter()
        .filter(|s| s.edge_kind == atlas_engine::DataFlowKind::FieldLoad)
        .map(|s| {
            case.store
                .get_data_node(&s.from_node_id)
                .unwrap()
                .unwrap()
                .name
                .unwrap()
        })
        .collect();
    assert_eq!(
        receivers,
        HashSet::from([
            "choose(object)".to_string(),
            "choose(object).child".to_string()
        ])
    );
    assert!(
        path.steps
            .iter()
            .any(|s| s.edge_kind == atlas_engine::DataFlowKind::ReturnToCall)
    );
    let unknown = Case::new(
        "struct Box { int slot; }; Box opaque(Box input); int selected(Box object) { return opaque(object).slot; }",
    );
    let path = unknown.trace("selected", 60);
    let source = path.source.data_node.as_ref().unwrap();
    assert_eq!(source.kind, DataNodeKind::CallReturn);
    assert_eq!(source.name.as_deref(), Some("opaque(object)"));
    assert!(
        path.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("trace_call_result_unavailable"))
    );
}

#[test]
fn field_store_traces_the_written_input_and_preserves_receiver_context() {
    for (source, expected) in [
        (
            "struct Box { int slot; }; void selected(Box* destination, int payload) { destination->slot = payload; }",
            "payload",
        ),
        (
            "struct Box { int slot; }; void selected(Box& destination, int original, int replacement) { original = replacement; destination.slot = original; }",
            "replacement",
        ),
        (
            "struct Box { int slot; }; void selected(Box* destination) { destination->slot = 7; }",
            "7",
        ),
        (
            "struct Box { int slot; }; int identity(int input) { return input; } void selected(Box* destination, int payload) { destination->slot = identity(payload); }",
            "payload",
        ),
    ] {
        let case = Case::new(source);
        let function = &case.store.find_symbols_by_qname("selected").unwrap()[0];
        let node = case
            .store
            .find_data_nodes_by_function(&function.id)
            .unwrap()
            .into_iter()
            .find(|n| {
                n.kind == DataNodeKind::Field
                    && case
                        .store
                        .find_dataflow_edges_by_target(&n.id)
                        .unwrap()
                        .iter()
                        .any(|e| e.kind == atlas_engine::DataFlowKind::FieldStore)
            })
            .unwrap();
        let before = case.store.find_dataflow_edges_by_target(&node.id).unwrap();
        let response = case.engine.trace_data_node(&node.id, 60, &[]);
        assert!(response.ok);
        let path = response.result.unwrap();
        assert_chain(&path);
        assert_eq!(
            path.source.data_node.as_ref().unwrap().name.as_deref(),
            Some(expected),
            "{source}"
        );
        assert!(
            path.steps
                .iter()
                .any(|s| s.edge_kind == atlas_engine::DataFlowKind::FieldStore)
        );
        assert!(
            !path
                .steps
                .iter()
                .any(|s| s.edge_kind == atlas_engine::DataFlowKind::FieldLoad)
        );
        let receiver = path
            .diagnostics
            .iter()
            .find(|d| d.code.as_deref() == Some("trace_field_receiver_context"))
            .unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(receiver.detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["at"]["data_node_id"], serde_json::json!(node.id));
        assert!(detail["total_edges"].as_u64().unwrap() > 0);
        assert!(
            detail["edges"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["edge_kind"] == "field_load" && e["position"]["location"].is_object())
        );
        assert_eq!(
            before,
            case.store.find_dataflow_edges_by_target(&node.id).unwrap()
        );
    }
}

#[test]
fn field_read_keeps_its_projection_without_borrowing_an_unrelated_store() {
    let case = Case::new(
        "struct Box { int slot; }; void writer(Box* destination, int payload) { destination->slot = payload; } int reader(Box* observed) { return observed->slot; }",
    );
    let path = case.trace("reader", 60);
    assert_eq!(
        path.source.data_node.as_ref().unwrap().name.as_deref(),
        Some("observed")
    );
    assert!(
        path.steps
            .iter()
            .any(|s| s.edge_kind == atlas_engine::DataFlowKind::FieldLoad)
    );
    assert!(
        !path
            .steps
            .iter()
            .any(|s| s.edge_kind == atlas_engine::DataFlowKind::FieldStore)
    );
    assert!(
        !path
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("trace_field_receiver_context"))
    );
}

#[test]
fn unnamed_parameters_do_not_shift_recorded_call_arguments() {
    let case = Case::new(
        r#"
int pick(int, int chosen) { return chosen; }
int selected(int wrong, int right) { int value = pick(wrong, right); return value; }
int unrelated(int other, int expected) { int value = pick(other, expected); return value; }
"#,
    );
    for (function, expected) in [("selected", "right"), ("unrelated", "expected")] {
        let path = case.trace(function, 60);
        assert_eq!(
            path.source.data_node.unwrap().name.as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn reference_pointer_and_defaulted_parameters_keep_explicit_argument_sources() {
    for source in [
        "int pick(const int& chosen) { return chosen; } int selected(int right) { int value = pick(right); return value; }",
        "int* pick(int* chosen) { return chosen; } int* selected(int* right) { return pick(right); }",
        "int pick(int chosen = 7) { return chosen; } int selected(int right) { int value = pick(right); return value; }",
    ] {
        let case = Case::new(source);
        let path = case.trace("selected", 60);
        assert_eq!(
            path.source.data_node.unwrap().name.as_deref(),
            Some("right"),
            "{source}"
        );
    }
    let case = Case::new(
        "int pick(int chosen = 7) { return chosen; } int selected() { int value = pick(); return value; }",
    );
    let path = case.trace("selected", 60);
    assert_eq!(
        path.source.data_node.unwrap().name.as_deref(),
        Some("chosen"),
        "the written default is not a substitute for a recorded argument mapping"
    );
}

#[test]
fn return_to_argument_stays_in_the_selected_invocation() {
    let case = Case::new(TWO_CALLERS);
    for (function, expected) in [("selected", "first"), ("unrelated", "second")] {
        let path = case.trace(function, 60);
        assert_eq!(
            path.source.data_node.as_ref().unwrap().name.as_deref(),
            Some(expected)
        );
        assert!(path.source.call_context.is_empty());
        assert!(path.steps.iter().any(|s| !s.call_context.is_empty()));
        assert!(
            path.diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some("trace_call_context_excluded"))
        );
    }
}

#[test]
fn repeated_calls_in_one_function_keep_distinct_callsites() {
    let case = Case::new(
        r#"
int identity(int input) { return input; }
int selected(int first, int second) {
    int value = identity(first);
    int other = identity(second);
    return value;
}
"#,
    );
    let path = case.trace("selected", 60);
    assert_eq!(
        path.source.data_node.unwrap().name.as_deref(),
        Some("first")
    );
}

#[test]
fn nested_calls_revisit_a_callee_node_in_different_invocations() {
    let case = Case::new(
        r#"
int identity(int input) { return input; }
// Qualified lookup isolates propagation from the unsupported ADL expression typing.
int selected(int first) { int value = ::identity(::identity(first)); return value; }
int unrelated(int second) { int value = identity(second); return value; }
"#,
    );
    let path = case.trace("selected", 60);
    assert_eq!(
        path.source.data_node.as_ref().unwrap().name.as_deref(),
        Some("first")
    );
    let parameter = case.node("identity", DataNodeKind::Parameter);
    let contexts: HashSet<_> = path
        .steps
        .iter()
        .filter(|s| s.from_node_id == parameter.id)
        .map(|s| s.call_context.clone())
        .collect();
    assert_eq!(
        contexts.len(),
        2,
        "both calls must be followed through the same callee parameter"
    );
}

#[test]
fn intermediate_calls_preserve_argument_order_and_constant_sources() {
    for function in ["relay", "forward"] {
        for (argument, expected) in [("first", "first"), ("7", "7")] {
            let source = format!(
                r#"
int right(int left, int right) {{ return right; }}
int {function}(int chosen, int ignored) {{ return right(ignored, chosen); }}
int selected(int first, int second) {{ int value = {function}({argument}, second); return value; }}
int unrelated(int third, int fourth) {{ int value = {function}(third, fourth); return value; }}
"#
            );
            let case = Case::new(&source);
            let path = case.trace("selected", 80);
            assert_eq!(
                path.source.data_node.unwrap().name.as_deref(),
                Some(expected)
            );
        }
    }
}

#[test]
fn deferred_branches_and_depth_continuations_keep_invocation_context() {
    let case = Case::new(
        r#"
int choose(int left, int right) { if (left) return left; return right; }
int selected(int first, int second) { int value = choose(first, second); return value; }
int unrelated(int third, int fourth) { int value = choose(third, fourth); return value; }
"#,
    );
    let mut pending = vec![case.trace("selected", 5)];
    let mut attempted = HashSet::new();
    let mut origins = HashSet::new();
    let mut scoped_continuations = 0;
    while let Some(path) = pending.pop() {
        assert_chain(&path);
        let source = path.source.data_node.as_ref().unwrap();
        if source.kind == DataNodeKind::Parameter && path.source.call_context.is_empty() {
            origins.insert(source.name.clone().unwrap());
        }
        for diagnostic in &path.diagnostics {
            if !matches!(
                diagnostic.code.as_deref(),
                Some("trace_alternatives_unexpanded" | "max_depth_truncated")
            ) {
                continue;
            }
            let detail: serde_json::Value =
                serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
            for edge in detail["edges"].as_array().unwrap() {
                let position = &edge["position"];
                if position.is_null() || position["call_context"].is_null() {
                    continue;
                }
                let node: DataNodeId =
                    serde_json::from_value(position["data_node_id"].clone()).unwrap();
                let context: Vec<CallsiteId> =
                    serde_json::from_value(position["call_context"].clone()).unwrap();
                if !attempted.insert((node, context.clone())) {
                    continue;
                }
                assert!(attempted.len() < 50, "bounded independent fixture");
                scoped_continuations += usize::from(!context.is_empty());
                let next = case.engine.trace_data_node(&node, 5, &context);
                assert!(next.ok);
                let next = next.result.unwrap();
                assert_eq!(next.sink.call_context, context);
                pending.push(next);
            }
        }
    }
    assert!(scoped_continuations > 0);
    assert_eq!(
        origins,
        HashSet::from(["first".to_owned(), "second".to_owned()])
    );
}

#[test]
fn invalid_context_stops_at_the_requested_node_without_resetting_it() {
    let case = Case::new(TWO_CALLERS);
    let node = case.node("selected", DataNodeKind::Return);
    let callsite = case.store.find_callsites_by_file(&node.file_id).unwrap()[0].id;
    // Either recorded call enters identity, never selected.
    let response = case.engine.trace_data_node(&node.id, 60, &[callsite]);
    assert!(response.ok && response.partial_result);
    let path = response.result.unwrap();
    assert!(path.steps.is_empty());
    assert_eq!(path.source.data_node.unwrap().id, node.id);
    assert_eq!(path.source.call_context, vec![callsite]);
    assert!(
        path.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("trace_call_context_unavailable"))
    );
}

#[test]
fn recursive_invocations_remain_bounded_without_becoming_an_intra_invocation_cycle() {
    let case = Case::new(
        r#"
int recurse(int input) { return recurse(input); }
int selected(int first) { int value = recurse(first); return value; }
"#,
    );
    let path = case.trace("selected", 25);
    assert_eq!(path.steps.len(), 25);
    assert!(path.source.call_context.len() > 1);
    assert!(
        path.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("max_depth_truncated"))
    );
}

fn all_parameter_origins(case: &Case, function: &str) -> HashSet<String> {
    let mut pending = vec![case.trace(function, 40)];
    let mut attempted = HashSet::new();
    let mut origins = HashSet::new();
    while let Some(path) = pending.pop() {
        assert_chain(&path);
        let source = path.source.data_node.as_ref().unwrap();
        if source.kind == DataNodeKind::Parameter && path.source.call_context.is_empty() {
            origins.insert(source.name.clone().unwrap());
        }
        for diagnostic in &path.diagnostics {
            if diagnostic.code.as_deref() != Some("trace_alternatives_unexpanded") {
                continue;
            }
            let detail: serde_json::Value =
                serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
            assert_eq!(detail["details_truncated"], false);
            for edge in detail["edges"].as_array().unwrap() {
                let position = &edge["position"];
                if position.is_null() || position["call_context"].is_null() {
                    continue;
                }
                let node: DataNodeId =
                    serde_json::from_value(position["data_node_id"].clone()).unwrap();
                let context: Vec<CallsiteId> =
                    serde_json::from_value(position["call_context"].clone()).unwrap();
                if attempted.insert((node, context.clone())) {
                    assert!(attempted.len() < 100, "bounded fixture continuation");
                    let response = case.engine.trace_data_node(&node, 40, &context);
                    assert!(response.ok);
                    pending.push(response.result.unwrap());
                }
            }
        }
    }
    origins
}

#[test]
fn call_results_do_not_inherit_ignored_arguments_in_alternative_paths() {
    for expression in ["relay(first, second)", "::relay(first, second)"] {
        let case = Case::new(&format!(
            "int right(int left, int right) {{ return right; }}\n\
             int relay(int chosen, int ignored) {{ return right(ignored, chosen); }}\n\
             int selected(int first, int second) {{ return {expression}; }}"
        ));
        assert_eq!(
            all_parameter_origins(&case, "selected"),
            HashSet::from(["first".to_owned()])
        );
    }
}

#[test]
fn compound_values_keep_noncall_operands_and_actual_argument_values() {
    for expression in [
        "::right(second, first) + third",
        "::right(second, first + third)",
    ] {
        let case = Case::new(&format!(
            "int right(int left, int right) {{ return right; }}\n\
             int selected(int first, int second, int third) {{ return {expression}; }}"
        ));
        assert_eq!(
            all_parameter_origins(&case, "selected"),
            HashSet::from(["first".to_owned(), "third".to_owned()])
        );
    }
}

#[test]
fn constant_callee_return_does_not_claim_argument_influence() {
    let case = Case::new(
        "int constant(int ignored) { return 42; }\n\
         int selected(int input) { return constant(input); }",
    );
    assert!(all_parameter_origins(&case, "selected").is_empty());
    assert_eq!(
        case.trace("selected", 40)
            .source
            .data_node
            .unwrap()
            .name
            .as_deref(),
        Some("42")
    );
}

#[test]
fn unavailable_return_source_stops_at_call_with_location_and_keeps_argument_queryable() {
    let case = Case::new(
        "int external(int);\n\
         int selected(int input) { return external(input); }",
    );
    let path = case.trace("selected", 40);
    assert_eq!(
        path.source.data_node.as_ref().unwrap().kind,
        DataNodeKind::CallReturn
    );
    assert!(path.partial_result);
    let diagnostic = path
        .diagnostics
        .iter()
        .find(|d| d.code.as_deref() == Some("trace_call_result_unavailable"))
        .unwrap();
    let detail: serde_json::Value =
        serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
    assert_eq!(
        detail["at"]["data_node_id"],
        serde_json::json!(path.source.data_node.unwrap().id)
    );
    let argument = case.node("selected", DataNodeKind::CallArg);
    let argument_trace = case.engine.trace_data_node(&argument.id, 20, &[]);
    assert_eq!(
        argument_trace
            .result
            .unwrap()
            .source
            .data_node
            .unwrap()
            .name
            .as_deref(),
        Some("input")
    );
}

#[test]
fn construction_result_does_not_borrow_its_arguments_or_nested_return_value() {
    for expression in [
        "new Item(input)",
        "new Item{input}",
        "new Item(::identity(input))",
    ] {
        let case = Case::new(&format!(
            "struct Item {{ Item(int ignored) {{}} }};\n\
             int identity(int value) {{ return value; }}\n\
             Item* selected(int input) {{ return {expression}; }}"
        ));
        assert!(
            all_parameter_origins(&case, "selected").is_empty(),
            "{expression}"
        );
        let path = case.trace("selected", 60);
        let source = path.source.data_node.as_ref().unwrap();
        assert_eq!(source.kind, DataNodeKind::CallReturn, "{expression}");
        assert_eq!(source.name.as_deref(), Some(expression));
        assert!(
            source.callsite_id.is_none(),
            "construction has no callee return mapping"
        );
        let gap = path
            .diagnostics
            .iter()
            .find(|d| d.code.as_deref() == Some("trace_call_result_unavailable"))
            .unwrap();
        let detail: serde_json::Value = serde_json::from_str(gap.detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["at"]["data_node_id"], serde_json::json!(source.id));
        // Constructor operands remain available for a separate investigation.
        let argument = case.node("selected", DataNodeKind::VariableUse);
        let trace = case
            .engine
            .trace_data_node(&argument.id, 40, &[])
            .result
            .unwrap();
        assert_eq!(
            trace.source.data_node.unwrap().name.as_deref(),
            Some("input")
        );
    }
}

#[test]
fn construction_used_as_an_argument_stops_before_internal_operands() {
    let case = Case::new(
        "struct Item { Item(int ignored) {} };\n\
         using Handle = Item*;\n\
         Handle keep(Handle value) { return value; }\n\
         Handle selected(int input) { return ::keep(new Item(input)); }",
    );
    assert!(all_parameter_origins(&case, "selected").is_empty());
    let path = case.trace("selected", 60);
    assert!(
        path.steps
            .iter()
            .any(|step| step.edge_kind == atlas_engine::DataFlowKind::ReturnToCall)
    );
    assert_eq!(
        path.source.data_node.unwrap().name.as_deref(),
        Some("new Item(input)")
    );
}

#[test]
fn conversion_syntax_is_not_an_ordinary_call_result_boundary() {
    for expression in [
        "static_cast<int>(input)",
        "int(input)",
        "int(static_cast<long>(input))",
        "static_cast<int>(::identity(input))",
    ] {
        let case = Case::new(&format!(
            "int identity(int value) {{ return value; }}\n\
             int selected(int input) {{ return {expression}; }}"
        ));
        assert_eq!(
            all_parameter_origins(&case, "selected"),
            HashSet::from(["input".to_owned()]),
            "{expression}"
        );
    }
}
