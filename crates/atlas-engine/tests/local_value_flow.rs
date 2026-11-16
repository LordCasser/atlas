use atlas_engine::{
    ExtractionMode, FileId, Language, Store,
    call_context::value_flow::{ValueFlowOptions, ValueSelection, trace_value},
    create_frontend, extract_file_with_mode,
};
use std::path::Path;

fn run(
    source: &str,
    needle: &str,
    depth: usize,
) -> atlas_engine::call_context::value_flow::ValueFlowResult {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("unit.cpp"), source).unwrap();
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("unit.cpp"),
        Path::new("unit.cpp"),
        source,
        &hash,
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    store.insert_file_facts(&facts).unwrap();
    let before = store.find_data_nodes_by_file(&facts.file.file_id).unwrap();
    let start = source.rfind(needle).unwrap() as u32;
    let result = trace_value(
        &store,
        dir.path(),
        &ValueSelection {
            path: "unit.cpp".into(),
            range: start..start + needle.len() as u32,
        },
        &ValueFlowOptions {
            max_depth: depth,
            max_paths: 8,
            max_functions: 1,
            call_context: vec![],
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(
        store.find_data_nodes_by_file(&facts.file.file_id).unwrap(),
        before,
        "Ready structural store remains unchanged"
    );
    assert_eq!(result.files_read, 1);
    result
}

#[test]
fn capture_initializer_values_use_the_enclosing_scope() {
    for source in [
        "void outer(int input) { auto pending = [saved = input + 1] {}; }",
        "void outer(int input) { auto pending = [saved = (input + 1)] {}; }",
        "void outer(int input) { auto pending = [&saved = input] {}; }",
        "void outer(int input) { auto pending = [nested = [saved = input + 1] {}] {}; }",
    ] {
        let selected = if source.contains("(input + 1)") {
            "(input + 1)"
        } else if source.contains("input + 1") {
            "input + 1"
        } else {
            "input"
        };
        let result = run(source, selected, 30);
        assert_eq!(result.scopes.len(), 1, "{source}: {result:#?}");
        assert_eq!(result.scopes[0].range.start_byte, 0, "{result:#?}");
        assert!(
            result.paths.iter().all(|path| {
                path.sink.location.range.start_byte as usize == source.rfind(selected).unwrap()
                    && path.sink.location.range.byte_len() as usize == selected.len()
            }),
            "the selected RHS must not fall back to the enclosing closure expression: {result:#?}"
        );
        assert!(
            result
                .paths
                .iter()
                .any(|path| path.endpoint.kind == "parameter"
                    && path.endpoint.name.as_deref() == Some("input")),
            "{source}: {result:#?}"
        );
        assert!(
            !result
                .gaps
                .iter()
                .any(|gap| gap.1 == "value_scope_unavailable"),
            "{result:#?}"
        );
    }
}

#[test]
fn capture_initializer_fields_keep_contents_separate_from_receivers() {
    let source = "struct Permissions { bool allowed; }; void outer(Permissions permissions) { auto pending = [allowed = permissions.allowed] {}; }";
    let result = run(source, "permissions.allowed", 30);
    assert_eq!(result.scopes.len(), 1, "{result:#?}");
    assert!(
        result
            .paths
            .iter()
            .all(|path| path.sink.location.range.byte_len() == 19),
        "{result:#?}"
    );
    assert!(
        result
            .paths
            .iter()
            .any(|path| path.endpoint.kind == "field"),
        "{result:#?}"
    );
    assert!(
        result
            .paths
            .iter()
            .all(|path| path.endpoint.kind != "parameter"),
        "a receiver is not the stored value: {result:#?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|gap| gap.1 == "value_field_contents_unestablished" && !gap.3.is_empty()),
        "{result:#?}"
    );
}

#[test]
fn capture_initializers_do_not_borrow_body_or_global_execution_contexts() {
    let body = run(
        "void outer(int input) { auto pending = [saved = input] { return saved; }; }",
        "saved",
        30,
    );
    assert_eq!(body.scopes.len(), 1, "{body:#?}");
    assert_ne!(body.scopes[0].range.start_byte, 0, "{body:#?}");
    assert!(
        body.paths
            .iter()
            .all(|path| path.endpoint.name.as_deref() != Some("input")),
        "creation is not invocation: {body:#?}"
    );
    for (source, selected) in [
        (
            "void outer(int input) { auto pending = [saved = input] {}; }",
            "saved",
        ),
        ("auto pending = [saved = 7] {};", "7"),
    ] {
        let result = run(source, selected, 30);
        assert!(
            result.scopes.is_empty() && result.paths.is_empty(),
            "{source}: {result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|gap| gap.1 == "value_scope_unavailable"),
            "{result:#?}"
        );
    }
    let nested = "void outer(int input) { auto pending = [](int local) { auto inner = [saved = local + 1] {}; }; }";
    let result = run(nested, "local + 1", 30);
    assert_eq!(result.scopes.len(), 1, "{result:#?}");
    assert_ne!(result.scopes[0].range.start_byte, 0, "{result:#?}");
    assert!(
        result
            .paths
            .iter()
            .any(|path| path.endpoint.kind == "parameter"
                && path.endpoint.name.as_deref() == Some("local")),
        "{result:#?}"
    );
}

#[test]
fn local_parameters_assignments_and_returns_have_readable_dependency_paths() {
    let result = run(
        "int pass(int input) { int value = input; return value; }",
        "value",
        30,
    );
    assert!(!result.paths.is_empty(), "{result:?}");
    assert!(
        result.paths.iter().any(|p| p.endpoint.kind == "parameter"
            && p.endpoint.name.as_deref() == Some("input")
            && !p.steps.is_empty()),
        "{result:?}"
    );
    assert!(result.gaps.iter().any(|g| g.1 == "value_flow_limited"));
}

#[test]
fn store_inputs_are_separate_from_field_receivers_and_unknown_call_results() {
    let stored = run(
        "struct Box { int value; }; void save(Box& box, int input) { box.value = input; }",
        "box.value",
        30,
    );
    assert!(
        stored
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("input")),
        "{stored:?}"
    );
    assert!(
        stored
            .gaps
            .iter()
            .any(|g| g.1 == "value_store_effect_unestablished")
    );
    let read = run(
        "struct Box { int value; }; int read(Box& box) { return box.value; }",
        "box.value",
        30,
    );
    assert!(
        read.gaps
            .iter()
            .any(|g| g.1 == "value_field_contents_unestablished" && !g.3.is_empty()),
        "{read:?}"
    );
    let returned = run(
        "int unknown(int); int read(int input) { int value = unknown(input); return value; }",
        "value",
        30,
    );
    assert!(
        returned
            .paths
            .iter()
            .all(|p| p.endpoint.kind != "parameter"),
        "a call result cannot borrow its argument: {returned:?}"
    );
    assert!(
        returned
            .gaps
            .iter()
            .any(|g| g.1 == "trace_call_result_unavailable"),
        "{returned:?}"
    );
}

#[test]
fn local_parameter_origin_does_not_require_an_invocation_slot() {
    let value = run(
        "struct Settings {}; int selected(int input, const Settings& settings = {}) { return input; }",
        "input",
        30,
    );
    assert!(!value.paths.is_empty(), "{value:#?}");
    assert!(
        value
            .paths
            .iter()
            .all(|p| p.endpoint.kind == "parameter" && p.endpoint.name.as_deref() == Some("input")),
        "{value:#?}"
    );
    assert!(value.gaps.iter().any(|g| g.1 == "value_extraction_limit"));
    let shadow = run(
        "struct Settings {}; int selected(int input, const Settings& settings = {}) { { int input = 7; return input; } }",
        "input",
        30,
    );
    assert!(!shadow.paths.is_empty(), "{shadow:#?}");
    assert!(
        shadow
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() == Some("7")),
        "{shadow:#?}"
    );
}

#[test]
fn field_reads_stop_before_receiver_origins_and_keep_receiver_continuation() {
    let address = run(
        "struct Box { int value; }; int* address(Box& box) { return &box.value; }",
        "&box.value",
        30,
    );
    assert!(
        address
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "parameter" && p.endpoint.name.as_deref() == Some("box")),
        "taking a field address must preserve object/address dependencies: {address:#?}"
    );
    for (source, selected) in [
        (
            "struct Box { int value; }; int read(Box& box) { return box.value; }",
            "box.value",
        ),
        (
            "struct Box { int value; }; int read(Box* box) { return box->value; }",
            "box->value",
        ),
        (
            "struct Box { int value; }; int read(Box& first, Box& second, int input) { first.value = input; return second.value; }",
            "second.value",
        ),
        (
            "struct Box { int value; }; void overwrite(Box&); int read(Box& box, int input) { box.value = input; overwrite(box); return box.value; }",
            "box.value",
        ),
    ] {
        let value = run(source, selected, 30);
        assert!(!value.paths.is_empty(), "{source}: {value:#?}");
        assert!(
            value.paths.iter().all(|p| p.endpoint.kind == "field"),
            "field contents cannot acquire the receiver or an unproven store as their origin: {source}: {value:#?}"
        );
        assert!(
            value
                .paths
                .iter()
                .all(|p| p.steps.iter().all(|s| s.kind != "field_load")),
            "{value:#?}"
        );
        let gap = value
            .gaps
            .iter()
            .find(|g| g.1 == "value_field_contents_unestablished")
            .unwrap();
        assert_eq!(
            &source[gap.0.range.start_byte as usize..gap.0.range.end_byte as usize],
            selected.rsplit(['.', '>']).next().unwrap()
        );
        assert!(
            !gap.3.is_empty(),
            "the actual receiver use remains independently selectable: {value:#?}"
        );
    }
    let case = CallsCase::new(&[(
        "calls.cpp",
        "struct Box { int value; }; Box* identity(Box* p) { return p; } int read(Box* input) { return identity(input)->value; }",
    )]);
    let read = case.trace(case.at("calls.cpp", "identity(input)->value"), vec![], 4);
    assert!(
        read.paths.iter().all(|p| p.endpoint.kind == "field"),
        "{read:#?}"
    );
    assert_eq!(
        read.scopes.len(),
        1,
        "a field read must not expand the address-producing function: {read:#?}"
    );
    let receiver = case.trace(case.at("calls.cpp", "identity(input)"), vec![], 4);
    assert!(
        receiver
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "parameter" && p.endpoint.name.as_deref() == Some("input")),
        "a separate receiver query must preserve its established parameter/return dependencies: {receiver:#?}"
    );
    let case = CallsCase::new(&[(
        "calls.cpp",
        "struct Box { int value; }; int fetched(Box* object) { return object->value; } int use(Box* external) { return fetched(external); }",
    )]);
    let read = case.trace(case.at("calls.cpp", "fetched(external)"), vec![], 4);
    let path = read
        .paths
        .iter()
        .find(|p| p.endpoint.kind == "field")
        .unwrap();
    assert_eq!(path.endpoint.call_context.len(), 1, "{read:#?}");
    assert!(
        read.paths.iter().all(|p| p.endpoint.kind != "parameter"),
        "{read:#?}"
    );
    let at = &read
        .gaps
        .iter()
        .find(|g| g.1 == "value_field_contents_unestablished")
        .unwrap()
        .3[0];
    let receiver = case.trace(
        ValueSelection {
            path: "calls.cpp".into(),
            range: at.range.start_byte..at.range.end_byte,
        },
        path.endpoint
            .call_context
            .iter()
            .map(|c| ValueSelection {
                path: "calls.cpp".into(),
                range: c.range.start_byte..c.range.end_byte,
            })
            .collect(),
        4,
    );
    assert!(
        receiver
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("external")
                && p.endpoint.call_context.is_empty()),
        "{receiver:#?}"
    );
}

#[test]
fn rebinding_branches_and_depth_limits_do_not_claim_an_exhaustive_origin() {
    let overwritten = run(
        "int pass(int input) { int value = input; value = 7; return value; }",
        "value",
        30,
    );
    assert!(
        overwritten
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() != Some("input")),
        "{overwritten:?}"
    );
    let branch = run(
        "int pass(int first, int second, bool choose) { int value; if (choose) value = first; else value = second; return value; }",
        "value",
        30,
    );
    assert!(
        branch
            .gaps
            .iter()
            .any(|g| g.1.contains("alternative") && !g.3.is_empty()),
        "{branch:?}"
    );
    let limited = run(
        "int pass(int input) { int first = input; int second = first; return second; }",
        "second",
        1,
    );
    assert!(limited.truncated, "{limited:?}");
}

#[test]
fn local_queries_bound_value_roles_and_reject_changed_sources_or_unowned_scopes() {
    let source = "int pass(int input) { return input; }";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("unit.cpp"), source).unwrap();
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("unit.cpp"),
        Path::new("unit.cpp"),
        source,
        blake3::hash(source.as_bytes()).to_hex().as_ref(),
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    store.insert_file_facts(&facts).unwrap();
    let start = source.rfind("input").unwrap() as u32;
    let query = |range, paths, cancel: &dyn Fn() -> bool| {
        trace_value(
            &store,
            dir.path(),
            &ValueSelection {
                path: "unit.cpp".into(),
                range,
            },
            &ValueFlowOptions {
                max_depth: 20,
                max_paths: paths,
                max_functions: 1,
                call_context: vec![],
            },
            cancel,
        )
    };
    let limited = query(start..start + 5, 1, &|| false).unwrap();
    assert_eq!(limited.paths.len(), 1);
    assert!(
        limited.truncated && limited.gaps.iter().any(|g| g.1 == "value_result_limit"),
        "{limited:?}"
    );
    let expanded = query(start..start + 5, 8, &|| false).unwrap();
    assert!(
        expanded.paths.len() > 1 && !expanded.truncated,
        "{expanded:?}"
    );
    let signature = query(4..8, 8, &|| false).unwrap();
    assert!(
        signature.paths.is_empty()
            && signature
                .gaps
                .iter()
                .any(|g| g.1 == "value_scope_unavailable")
    );
    assert!(query(start..start + 5, 8, &|| true).is_err());
    std::fs::write(dir.path().join("unit.cpp"), "int changed;\n").unwrap();
    let changed = query(start..start + 5, 8, &|| false).unwrap();
    assert!(changed.paths.is_empty() && changed.gaps.iter().any(|g| g.1 == "value_source_changed"));
}

struct CallsCase {
    dir: tempfile::TempDir,
    store: std::sync::Arc<Store>,
    sources: std::collections::BTreeMap<String, String>,
}

#[test]
fn selected_calls_use_visible_default_expressions_without_replacing_explicit_arguments() {
    let case = CallsCase::new(&[
        (
            "api.h",
            "struct Gate { static bool check(bool local = false); };\n",
        ),
        (
            "impl.cpp",
            "#include \"api.h\"\nbool Gate::check(bool local) { return local; }\n",
        ),
        (
            "main.cpp",
            "#include \"api.h\"\nbool run() { return Gate::check(); }\nbool explicit_call() { return Gate::check(true); }\n",
        ),
    ]);
    let result = case.trace(
        case.at("impl.cpp", "local"),
        vec![case.at("main.cpp", "Gate::check()")],
        3,
    );
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "default_argument"
                && p.endpoint.name.as_deref() == Some("false")
                && p.endpoint.location.file_id == FileId::generate("api.h")),
        "{result:#?}"
    );
    assert!(
        result.paths.iter().any(
            |p| p.steps.first().is_some_and(|s| s.kind == "default_argument"
                && s.to.kind == "parameter"
                && s.to.call_context.len() == 1
                && s.from.call_context.is_empty())
        ),
        "{result:#?}"
    );
    assert!(
        !result
            .gaps
            .iter()
            .any(|g| g.1 == "value_argument_unavailable"),
        "{result:#?}"
    );
    let explicit = case.trace(
        case.at("impl.cpp", "local"),
        vec![case.at("main.cpp", "Gate::check(true)")],
        3,
    );
    assert!(
        explicit
            .paths
            .iter()
            .all(|p| p.endpoint.kind != "default_argument")
    );
    assert!(
        explicit
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("true")),
        "{explicit:#?}"
    );
}

#[test]
fn default_inputs_keep_expression_boundaries_and_redeclaration_ambiguity() {
    let case = CallsCase::new(&[(
        "unit.cpp",
        "int unknown(); int read(int input = unknown()) { return input; }\nint run() { return read(); }\n",
    )]);
    let result = case.trace(
        case.at("unit.cpp", "input"),
        vec![case.at("unit.cpp", "read()")],
        2,
    );
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("unknown()")
                && p.endpoint.kind == "default_argument"),
        "{result:#?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.1 == "value_default_expression_unexpanded")
    );

    let ambiguous = CallsCase::new(&[(
        "unit.cpp",
        "bool check(bool x = false);\nbool check(bool x = true);\nbool check(bool x) { return x; }\nbool run() { return check(); }\n",
    )]);
    let result = ambiguous.trace(
        ambiguous.at("unit.cpp", "x"),
        vec![ambiguous.at("unit.cpp", "check()")],
        2,
    );
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.kind != "default_argument"),
        "{result:#?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.1 == "value_default_argument_ambiguous"),
        "{result:#?}"
    );
}

#[test]
fn default_inputs_do_not_borrow_another_scope_or_an_unselected_invocation() {
    let case = CallsCase::new(&[
        ("api.h", "bool check(bool local = false);\n"),
        ("hidden.h", "bool check(bool local = true);\n"),
        (
            "impl.cpp",
            "#include \"api.h\"\nbool check(bool local) { return local; }\n",
        ),
        (
            "main.cpp",
            "#include \"api.h\"\nbool run() { return check(); }\n#include \"hidden.h\"\n",
        ),
    ]);
    let selected = case.trace(
        case.at("impl.cpp", "local"),
        vec![case.at("main.cpp", "check()")],
        3,
    );
    assert!(
        selected
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("false")
                && p.endpoint.kind == "default_argument"),
        "{selected:#?}"
    );
    assert!(
        selected
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() != Some("true"))
    );
    let unselected = case.trace(case.at("impl.cpp", "local"), vec![], 3);
    assert!(
        unselected
            .paths
            .iter()
            .all(|p| p.endpoint.kind != "default_argument")
    );
    // An unrelated translation unit's default cannot supply an omitted input.
    let unavailable = CallsCase::new(&[
        ("api.h", "bool check(bool local);\n"),
        (
            "impl.cpp",
            "bool check(bool local = true) { return local; }\n",
        ),
        (
            "main.cpp",
            "#include \"api.h\"\nbool run() { return check(); }\n",
        ),
    ]);
    let result = unavailable.trace(
        unavailable.at("impl.cpp", "local"),
        vec![unavailable.at("main.cpp", "check()")],
        3,
    );
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.kind != "default_argument")
    );
    assert!(!result.gaps.is_empty());
}
impl CallsCase {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let mut ids = vec![];
        for (path, source) in files {
            std::fs::write(dir.path().join(path), source).unwrap();
            let id = FileId::generate(path);
            ids.push(id);
            let facts = extract_file_with_mode(
                &create_frontend(Language::Cpp).unwrap(),
                id,
                Path::new(path),
                source,
                blake3::hash(source.as_bytes()).to_hex().as_ref(),
                ExtractionMode::Structural,
                &(),
            )
            .unwrap();
            store.insert_file_facts(&facts).unwrap();
        }
        atlas_engine::ReferenceResolver::new(store.clone())
            .resolve_for_files(&ids)
            .unwrap();
        Self {
            dir,
            store,
            sources: files
                .iter()
                .map(|(p, s)| (p.to_string(), s.to_string()))
                .collect(),
        }
    }
    fn at(&self, path: &str, needle: &str) -> ValueSelection {
        let start = self.sources[path].rfind(needle).unwrap() as u32;
        ValueSelection {
            path: path.into(),
            range: start..start + needle.len() as u32,
        }
    }
    fn trace(
        &self,
        at: ValueSelection,
        context: Vec<ValueSelection>,
        functions: usize,
    ) -> atlas_engine::call_context::value_flow::ValueFlowResult {
        let before: Vec<_> = self
            .sources
            .keys()
            .map(|p| {
                self.store
                    .find_data_nodes_by_file(&FileId::generate(p))
                    .unwrap()
            })
            .collect();
        let result = trace_value(
            &self.store,
            self.dir.path(),
            &at,
            &ValueFlowOptions {
                max_depth: 60,
                max_paths: 8,
                max_functions: functions,
                call_context: context,
            },
            &|| false,
        )
        .unwrap();
        let after: Vec<_> = self
            .sources
            .keys()
            .map(|p| {
                self.store
                    .find_data_nodes_by_file(&FileId::generate(p))
                    .unwrap()
            })
            .collect();
        assert_eq!(before, after);
        result
    }
}

#[test]
fn local_read_does_not_spend_its_budget_on_unused_reference_exit_summaries() {
    let parameters = (0..8)
        .map(|i| format!("int& unused{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut source = format!("int run({parameters}, int input, int mode) {{\n");
    for i in 0..200 {
        source.push_str(&format!("if (mode == {i}) return {i};\n"));
    }
    source.push_str("int value = input; return value; }\n");
    let case = CallsCase::new(&[("many_returns.cpp", &source)]);
    let result = case.trace(case.at("many_returns.cpp", "value"), vec![], 4);
    assert!(!result.truncated, "{result:#?}");
    assert!(!result.paths.is_empty());
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() == Some("input"))
    );
    assert!(
        !result
            .gaps
            .iter()
            .any(|g| g.1 == "value_writeback_exit_alternatives")
    );
}

#[test]
fn boolean_guards_select_reference_exits_through_direct_return_wrappers() {
    for (wrapper, guard, wanted) in [
        ("return fill(out, c);", "!relay(result, choose)", "7"),
        ("return fill(out, c);", "relay(result, choose)", "9"),
        ("return !fill(out, c);", "relay(result, choose)", "7"),
    ] {
        let source = format!(
            "bool fill(int& output, bool choose) {{ if (choose) {{ output = 7; return true; }} output = 9; return false; }} bool relay(int& out, bool c) {{ {wrapper} }} int run(bool choose) {{ int result = 0; if ({guard}) return -1; return result; }}\n"
        );
        let case = CallsCase::new(&[("guard.cpp", &source)]);
        let result = case.trace(case.at("guard.cpp", "result"), vec![], 4);
        assert!(!result.truncated && !result.paths.is_empty(), "{result:#?}");
        assert!(
            result
                .paths
                .iter()
                .all(|p| p.endpoint.name.as_deref() == Some(wanted)),
            "{wrapper} {guard}: {result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.1 == "value_return_condition_excluded" && g.3.len() >= 2),
            "{result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.1 == "value_writeback_exit_alternatives")
        );
    }
}

#[test]
fn return_constraints_keep_repeated_invocations_separate() {
    let source = "bool pick(int& out, int input, bool ok) { if (ok) { out = input; return true; } out = input + 1; return false; } int run(int wanted, bool a, bool b) { int first = 0; if (!pick(first, wanted, a)) return -1; int result = 0; if (pick(result, first, b)) return -2; return result; }\n";
    let case = CallsCase::new(&[("repeated.cpp", source)]);
    let result = case.trace(case.at("repeated.cpp", "result"), vec![], 4);
    assert!(!result.truncated && !result.paths.is_empty(), "{result:#?}");
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() == Some("wanted")),
        "{result:#?}"
    );
    for path in &result.paths {
        let exits: Vec<_> = path
            .steps
            .iter()
            .filter(|s| s.kind == "writeback_to_call")
            .collect();
        assert_eq!(exits.len(), 2, "{path:#?}");
        let observed: Vec<_> = exits
            .iter()
            .map(|s| {
                let exit = s.from.location.range;
                let call = s.from.call_context.last().unwrap().range;
                (
                    &source[exit.start_byte as usize..exit.end_byte as usize],
                    &source[call.start_byte as usize..call.end_byte as usize],
                )
            })
            .collect();
        assert!(
            observed.contains(&("return true;", "pick(first, wanted, a)")),
            "{observed:?}"
        );
        assert!(
            observed.contains(&("return false;", "pick(result, first, b)")),
            "{observed:?}"
        );
    }
}

#[test]
fn return_constraints_preserve_unrelated_complex_and_unknown_conditions() {
    for tail in [
        "fill(result, choose); if (!permit) return -1; return result;",
        "if (!(fill(result, choose) || permit)) return -1; return result;",
        "if (!fill(result, choose)) {} return result;",
        "fill(result, choose); if (!fill(other, choose)) return -1; return result;",
        "\n#if VARIANT\n goto done;\n#endif\n if (!fill(result, choose)) return -1; done: return result;",
    ] {
        let source = format!(
            "bool fill(int& out, bool choose) {{ if (choose) {{ out = 7; return true; }} out = 9; return false; }} int run(bool choose, bool permit) {{ int result = 0; int other = 1; {tail} }}\n"
        );
        let case = CallsCase::new(&[("unknown.cpp", &source)]);
        let result = case.trace(case.at("unknown.cpp", "result"), vec![], 4);
        assert!(!result.truncated && !result.paths.is_empty(), "{result:#?}");
        assert!(
            !result
                .gaps
                .iter()
                .any(|g| g.1 == "value_return_condition_excluded"),
            "{tail}: {result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.1 == "value_writeback_exit_alternatives")
        );
    }
    // A bool wrapper need not return the result of the call whose output is read.
    let source = "bool fill(int& out) { out = 9; return false; } bool relay(int& out) { fill(out); return true; } int run() { int result = 0; if (!relay(result)) return -1; return result; }\n";
    let case = CallsCase::new(&[("wrapper.cpp", source)]);
    let result = case.trace(case.at("wrapper.cpp", "result"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("9")),
        "{result:#?}"
    );
    assert!(
        !result
            .gaps
            .iter()
            .any(|g| g.1 == "value_return_condition_excluded")
    );
}

#[test]
fn non_boolean_results_and_unknown_returns_do_not_justify_exit_exclusion() {
    for source in [
        "struct Flag { Flag(bool); bool operator!() const; }; Flag fill(int& out) { out = 9; return false; } int run() { int result = 0; if (!fill(result)) return -1; return result; }\n",
        "bool fill(int& out, bool choose) { out = 9; return choose; } int run(bool choose) { int result = 0; if (!fill(result, choose)) return -1; return result; }\n",
    ] {
        let case = CallsCase::new(&[("returns.cpp", source)]);
        let result = case.trace(case.at("returns.cpp", "result"), vec![], 4);
        assert!(!result.truncated && !result.paths.is_empty(), "{result:#?}");
        assert!(
            !result
                .gaps
                .iter()
                .any(|g| g.1 == "value_return_condition_excluded"),
            "{result:#?}"
        );
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.endpoint.name.as_deref() == Some("9")),
            "{result:#?}"
        );
    }
}

#[test]
fn conflicting_all_recorded_exits_remain_a_located_stop() {
    let source = "bool fill(int& output) { output = 9; return false; } int run() { int result = 0; if (!fill(result)) return -1; return result; }\n";
    let case = CallsCase::new(&[("conflict.cpp", source)]);
    let result = case.trace(case.at("conflict.cpp", "result"), vec![], 4);
    assert!(!result.truncated && !result.paths.is_empty(), "{result:#?}");
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.kind == "call_output"),
        "{result:#?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.1 == "value_return_condition_excluded")
    );
    assert!(result.gaps.iter().any(|g| g.1 == "value_flow_limited"));
}

#[test]
fn reference_exit_alternatives_locate_returns_and_their_own_writes() {
    let source = "bool fill(int& output, bool choose) { if (choose) { output = 7; return true; } output = 9; return false; } int run(bool choose) { int result = 0; fill(result, choose); return result; }\n";
    let case = CallsCase::new(&[("exits.cpp", source)]);
    let result = case.trace(case.at("exits.cpp", "result"), vec![], 4);
    let exits: Vec<_> = result
        .gaps
        .iter()
        .filter(|g| g.1 == "value_writeback_exit_alternatives")
        .collect();
    assert_eq!(exits.len(), 2, "{result:#?}");
    for (returned, write, other) in [
        ("return true;", "output = 7", "output = 9"),
        ("return false;", "output = 9", "output = 7"),
    ] {
        let gap = exits
            .iter()
            .find(|g| {
                &source[g.0.range.start_byte as usize..g.0.range.end_byte as usize] == returned
            })
            .unwrap();
        assert!(
            gap.3
                .iter()
                .any(|loc| loc.range.start_byte as usize == source.find(write).unwrap()),
            "{gap:?}"
        );
        assert!(
            !gap.3
                .iter()
                .any(|loc| loc.range.start_byte as usize == source.find(other).unwrap()),
            "{gap:?}"
        );
    }
    assert!(!result.truncated);
    assert!(result.paths.iter().any(|p| p.steps.iter().any(|s| {
        s.kind == "writeback_to_call"
            && s.from.kind == "parameter_output"
            && source
                [s.from.location.range.start_byte as usize..s.from.location.range.end_byte as usize]
                .starts_with("return ")
            && s.from.call_context.len() == 1
    })));
}

#[test]
fn reference_exit_values_follow_nested_and_repeated_invocations() {
    for source in [
        "void set(int& output, int input) { output = input; } bool relay(int& destination, int selected) { set(destination, selected); return true; } int run(int wanted, int other) { int result = 0; relay(result, wanted); return result; }\n",
        "void set(int& output, int input) { output = input; } int run(int other, int wanted) { int result = 0; set(result, other); set(result, wanted); return result; }\n",
        "void set(int& output, int first, int last) { output = first; output = last; } int run(int other, int wanted) { int result = 0; set(result, other, wanted); return result; }\n",
        "void set(int& output, int input) { { int output = 99; } output = input; } int run(int wanted) { int result = 0; set(result, wanted); return result; }\n",
    ] {
        let case = CallsCase::new(&[("writes.cpp", source)]);
        let result = case.trace(case.at("writes.cpp", "result"), vec![], 4);
        assert!(!result.paths.is_empty(), "{result:#?}");
        assert!(
            result.paths.iter().all(|p| p.endpoint.kind == "parameter"
                && p.endpoint.name.as_deref() == Some("wanted")
                && p.endpoint.call_context.is_empty()),
            "{result:#?}"
        );
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.steps.iter().any(|s| s.kind == "writeback_to_call"
                    && s.from.call_context.len() == s.to.call_context.len() + 1)),
            "{result:#?}"
        );
        assert!(
            !result
                .gaps
                .iter()
                .any(|g| g.1 == "trace_call_output_unavailable"),
            "{result:#?}"
        );
        assert!(!result.truncated, "{result:#?}");
    }
}

#[test]
fn reference_exit_alternatives_and_exceptional_exits_remain_distinct() {
    for source in [
        "void set(int& output, int first, int second, bool choose) { if (choose) output = first; else output = second; } int run(int first, int second, bool choose) { int result = 0; set(result, first, second, choose); return result; }\n",
        "void set(int& output, int input, bool choose) { if (choose) output = input; } int run(int input, bool choose) { int result = 0; set(result, input, choose); return result; }\n",
    ] {
        let case = CallsCase::new(&[("branch.cpp", source)]);
        let result = case.trace(case.at("branch.cpp", "result"), vec![], 4);
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.steps.iter().any(|s| s.kind == "writeback_to_call")),
            "{result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.1 == "trace_alternatives_unexpanded" && !g.3.is_empty()),
            "{result:#?}"
        );
    }
    let case = CallsCase::new(&[(
        "throw.cpp",
        "void set(int& output, bool fail) { if (fail) { output = 99; throw 1; } output = 7; } int run(bool fail) { int result = 0; set(result, fail); return result; }\n",
    )]);
    let result = case.trace(case.at("throw.cpp", "result"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("7")),
        "{result:#?}"
    );
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() != Some("99")),
        "{result:#?}"
    );
    let case = CallsCase::new(&[(
        "throw.cpp",
        "void set(int& output) { output = 99; throw 1; } int run() { int result = 0; set(result); return result; }\n",
    )]);
    let result = case.trace(case.at("throw.cpp", "result"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "call_output"),
        "{result:#?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.1 == "value_writeback_exit_unavailable"),
        "{result:#?}"
    );
}
#[test]
fn known_call_return_and_argument_mapping_preserve_the_selected_invocation() {
    let case = CallsCase::new(&[
        ("callee.cpp", "int identity(int input) { return input; }"),
        (
            "caller.cpp",
            "int identity(int); int selected(int first) { int value = identity(first); return value; } int unrelated(int second) { return identity(second); }",
        ),
    ]);
    let result = case.trace(case.at("caller.cpp", "value"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("first")),
        "{result:?}"
    );
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() != Some("second")),
        "{result:?}"
    );
    assert_eq!(result.scopes.len(), 2);
    let kinds: Vec<_> = result
        .paths
        .iter()
        .flat_map(|p| &p.steps)
        .map(|s| s.kind.as_str())
        .collect();
    assert!(
        kinds.contains(&"return_to_call") && kinds.contains(&"arg_to_param"),
        "{result:?}"
    );
    assert!(
        result
            .paths
            .iter()
            .flat_map(|p| &p.steps)
            .any(|s| s.from.location.file_id != s.to.location.file_id)
    );
    for (selected, expected) in [("identity(first)", "first"), ("identity(second)", "second")] {
        let result = case.trace(
            case.at("callee.cpp", "input"),
            vec![case.at("caller.cpp", selected)],
            4,
        );
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.endpoint.name.as_deref() == Some(expected)),
            "{result:?}"
        );
        assert!(result.paths.iter().all(|p| p.sink.call_context.len() == 1));
    }
    let unselected = case.trace(case.at("callee.cpp", "input"), vec![], 4);
    assert!(
        unselected
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() == Some("input"))
    );
    assert_eq!(unselected.scopes.len(), 1);
    let mismatch = case.trace(
        case.at("caller.cpp", "value"),
        vec![case.at("caller.cpp", "identity(second)")],
        4,
    );
    assert!(
        mismatch.paths.is_empty()
            && mismatch
                .gaps
                .iter()
                .any(|g| g.1 == "value_call_context_mismatch")
    );
}

#[test]
fn call_outputs_trace_reference_writes_without_replacing_pre_call_or_value_inputs() {
    let case = CallsCase::new(&[(
        "unit.cpp",
        "void fill(int& output) { output = 7; }\nvoid copy(int input) { input = 8; }\nint changed() { int value = 0; int before = value; fill(value); return value; }\nint unchanged() { int retained = 1; copy(retained); return retained; }\nint overwritten() { int replaced = 2; fill(replaced); replaced = 9; return replaced; }\n",
    )]);
    let changed = case.trace(case.at("unit.cpp", "value"), vec![], 4);
    assert!(
        changed
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "literal" && p.endpoint.name.as_deref() == Some("7")),
        "{changed:#?}"
    );
    assert!(
        changed
            .paths
            .iter()
            .all(|p| p.endpoint.name.as_deref() != Some("0")),
        "{changed:#?}"
    );
    assert!(
        !changed
            .gaps
            .iter()
            .any(|g| g.1 == "trace_call_output_unavailable"),
        "{changed:#?}"
    );
    assert!(
        changed
            .paths
            .iter()
            .any(|p| p.steps.iter().any(|s| s.kind == "writeback_to_call"))
    );
    let before = case.trace(case.at("unit.cpp", "before"), vec![], 4);
    assert!(
        before
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("0")),
        "{before:#?}"
    );
    let unchanged = case.trace(case.at("unit.cpp", "retained"), vec![], 4);
    assert!(
        unchanged
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("1")),
        "{unchanged:#?}"
    );
    assert!(
        !unchanged
            .gaps
            .iter()
            .any(|g| g.1 == "trace_call_output_unavailable"),
        "{unchanged:#?}"
    );
    let overwritten = case.trace(case.at("unit.cpp", "replaced"), vec![], 4);
    assert!(
        overwritten
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("9")),
        "{overwritten:#?}"
    );
    assert!(
        !overwritten
            .gaps
            .iter()
            .any(|g| g.1 == "trace_call_output_unavailable"),
        "{overwritten:#?}"
    );
}
#[test]
fn nested_calls_argument_order_and_repeated_calls_do_not_borrow_another_input() {
    for source in [
        "int pick(int ignored, int chosen) { return chosen; } int wrap(int left, int right) { return pick(right, left); } int selected(int first, int second) { int value = wrap(first, second); return value; }",
        "int identity(int input) { return input; } int selected(int first, int second) { identity(second); int value = identity(first); return value; }",
    ] {
        let case = CallsCase::new(&[("calls.cpp", source)]);
        let result = case.trace(case.at("calls.cpp", "value"), vec![], 4);
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.endpoint.name.as_deref() == Some("first")),
            "{result:?}"
        );
        assert!(
            result
                .paths
                .iter()
                .all(|p| p.endpoint.name.as_deref() != Some("second")),
            "{result:?}"
        );
    }
}

#[test]
fn parameter_declarations_are_entry_inputs_without_later_call_effects() {
    for declaration in [
        "int& value",
        "int *value",
        "int (&value)",
        "int (*value)(int)",
    ] {
        let source = format!("void run({declaration}) {{ missing(value); consume(value); }}\n");
        let case = CallsCase::new(&[("unit.cpp", &source)]);
        let start = source.find("value").unwrap() as u32;
        let entry = case.trace(
            ValueSelection {
                path: "unit.cpp".into(),
                range: start..start + 5,
            },
            vec![],
            1,
        );
        assert!(!entry.paths.is_empty(), "{source}\n{entry:#?}");
        assert!(
            entry.paths.iter().all(|path| {
                path.sink.kind == "parameter"
                    && path.endpoint.kind == "parameter"
                    && path.endpoint.location.range.start_byte == start
                    && path.steps.is_empty()
            }),
            "{source}\n{entry:#?}"
        );
        assert!(
            !entry
                .gaps
                .iter()
                .any(|gap| gap.1 == "value_call_effect_unestablished"),
            "later calls cannot affect an entry declaration: {source}\n{entry:#?}"
        );

        let used = case.trace(case.at("unit.cpp", "value"), vec![], 1);
        assert!(
            used.paths
                .iter()
                .any(|path| path.endpoint.kind == "parameter"
                    && path.endpoint.location.range.start_byte == start),
            "{source}\n{used:#?}"
        );
        assert!(
            used.gaps
                .iter()
                .any(|gap| gap.1 == "value_call_effect_unestablished"),
            "the prior unknown call still affects the later use: {source}\n{used:#?}"
        );
    }
}

#[test]
fn default_expression_identifier_is_not_a_parameter_entry() {
    let source = "int initial = 7; void run(int value = initial) { consume(value); }\n";
    let case = CallsCase::new(&[("unit.cpp", source)]);
    let value = case.trace(case.at("unit.cpp", "initial"), vec![], 1);
    assert!(!value.paths.is_empty(), "{value:#?}");
    assert!(
        value
            .paths
            .iter()
            .all(|path| path.sink.kind != "parameter" && path.endpoint.kind != "parameter"),
        "a default expression is not another input: {value:#?}"
    );
}

#[test]
fn call_outputs_keep_address_alias_unknown_and_conditional_effects_located() {
    let cast = CallsCase::new(&[(
        "unit.cpp",
        "int run() { int value = 4; static_cast<unsigned>(value); return value; }\n",
    )]);
    let cast_result = cast.trace(cast.at("unit.cpp", "value"), vec![], 4);
    assert!(
        cast_result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("4")),
        "{cast_result:#?}"
    );
    assert!(
        !cast_result
            .gaps
            .iter()
            .any(|g| g.1 == "trace_call_output_unavailable"),
        "{cast_result:#?}"
    );
    for source in [
        "void update(int* out); int run() { int value = 0; update(&value); return value; }\n",
        "void update(int& out); int run(bool condition) { int value = 0; if (condition) update(value); return value; }\n",
        // Const-reference syntax alone does not rule out const_cast on a mutable object.
        "void observe(const int& out); int run() { int value = 0; observe(value); return value; }\n",
        "void update(int& out); int run() { int value = 0; update((value /* same binding */)); return value; }\n",
    ] {
        let case = CallsCase::new(&[("unit.cpp", source)]);
        let result = case.trace(case.at("unit.cpp", "value"), vec![], 4);
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.endpoint.kind == "call_output"),
            "{source}\n{result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.1 == "trace_call_output_unavailable" && !g.3.is_empty()),
            "{source}\n{result:#?}"
        );
    }
    for (source, call) in [
        (
            "using Ref = int&; void update(Ref out); int run() { int value = 0; update(value); return value; }\n",
            "update(value)",
        ),
        (
            "int run() { int value = 0; missing(value); return value; }\n",
            "missing(value)",
        ),
    ] {
        let case = CallsCase::new(&[("unit.cpp", source)]);
        let result = case.trace(case.at("unit.cpp", "value"), vec![], 4);
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.endpoint.name.as_deref() == Some("0")),
            "{result:#?}"
        );
        assert!(
            !result
                .paths
                .iter()
                .any(|p| p.endpoint.kind == "call_output"),
            "{result:#?}"
        );
        let gap = result
            .gaps
            .iter()
            .find(|g| g.1 == "value_call_effect_unestablished")
            .expect("unknown effect must qualify the displayed earlier definition");
        assert_eq!(
            &source[gap.0.range.start_byte as usize..gap.0.range.end_byte as usize],
            "value"
        );
        assert!(
            gap.3.iter().any(|at| &source
                [at.range.start_byte as usize..at.range.end_byte as usize]
                == call),
            "{gap:#?}"
        );
    }
    for source in [
        "int run() { int value = 0; missing(value); value = 9; return value; }\n",
        "int run() { int value = 9; int unrelated = 0; missing(unrelated); return value; }\n",
    ] {
        let case = CallsCase::new(&[("unit.cpp", source)]);
        let result = case.trace(case.at("unit.cpp", "value"), vec![], 4);
        assert!(
            result
                .paths
                .iter()
                .any(|p| p.endpoint.name.as_deref() == Some("9")),
            "{result:#?}"
        );
        assert!(
            !result
                .gaps
                .iter()
                .any(|g| g.1 == "value_call_effect_unestablished"),
            "{result:#?}"
        );
    }
    let source = "void update(int&); int run() { int value = 3; { int value = 0; update(value); } return value; }\n";
    let case = CallsCase::new(&[("unit.cpp", source)]);
    let result = case.trace(case.at("unit.cpp", "value"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("3")),
        "{result:#?}"
    );
    assert!(
        !result
            .gaps
            .iter()
            .any(|g| g.1 == "trace_call_output_unavailable"),
        "{result:#?}"
    );
}
#[test]
fn function_budgets_missing_bodies_and_return_alternatives_remain_located() {
    let case = CallsCase::new(&[(
        "calls.cpp",
        "int choose(int left, int right, bool condition) { if (condition) return left; return right; } int selected(int first, int second, bool c) { int value = choose(first, second, c); return value; }",
    )]);
    let at = case.at("calls.cpp", "value");
    let bounded = case.trace(at.clone(), vec![], 1);
    assert!(
        bounded.truncated
            && bounded
                .gaps
                .iter()
                .any(|g| g.1 == "value_function_limit" && !g.3.is_empty()),
        "{bounded:?}"
    );
    assert!(
        bounded
            .paths
            .iter()
            .all(|p| p.endpoint.kind == "call_return")
    );
    let result = case.trace(at, vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("second")),
        "{result:?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.1 == "trace_alternatives_unexpanded" && !g.3.is_empty()),
        "{result:?}"
    );
    let missing = CallsCase::new(&[(
        "calls.cpp",
        "int external(int); int selected(int first) { int value = external(first); return value; }",
    )]);
    let result = missing.trace(missing.at("calls.cpp", "value"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.kind == "call_return"),
        "{result:?}"
    );
    assert!(
        result.gaps.iter().any(|g| g.1 == "value_body_unavailable"),
        "{result:?}"
    );
}

#[test]
fn default_arguments_continue_and_recursive_contexts_stay_bounded() {
    let missing = CallsCase::new(&[(
        "calls.cpp",
        "int defaulted(int input = 7) { return input; } int selected() { int value = defaulted(); return value; }",
    )]);
    let result = missing.trace(missing.at("calls.cpp", "value"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "default_argument"
                && p.endpoint.name.as_deref() == Some("7")),
        "{result:?}"
    );
    let limited = trace_value(
        &missing.store,
        missing.dir.path(),
        &missing.at("calls.cpp", "input"),
        &ValueFlowOptions {
            max_depth: 1,
            max_paths: 8,
            max_functions: 2,
            call_context: vec![missing.at("calls.cpp", "defaulted()")],
        },
        &|| false,
    )
    .unwrap();
    assert!(limited.paths.iter().all(|p| p.steps.len() <= 1));
    assert!(
        limited.truncated
            && limited
                .gaps
                .iter()
                .any(|g| g.1 == "value_default_depth_limit"),
        "{limited:#?}"
    );
    // The context budget requires an indexed call. A whole-file function
    // without a final newline currently loses its caller in structural extraction;
    // keep that independent miss below instead of inventing a call for the query.
    let recursive = CallsCase::new(&[(
        "calls.cpp",
        "int recursive(int input) { return recursive(input); }\n",
    )]);
    let at = recursive.at("calls.cpp", "recursive(input)");
    let result = recursive.trace(at.clone(), vec![at; 60], 4);
    assert!(
        result.truncated && result.gaps.iter().any(|g| g.1 == "value_context_budget"),
        "{result:?}; calls: {:?}",
        recursive
            .store
            .find_callsites_by_file(&FileId::generate("calls.cpp"))
            .unwrap()
    );
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.call_context.len() <= 100 && !p.endpoint.call_context.is_empty()),
        "{result:?}"
    );
}

#[test]
fn an_unrecorded_recursive_call_is_not_invented_as_query_context() {
    let case = CallsCase::new(&[(
        "calls.cpp",
        "int recursive(int input) { return recursive(input); }",
    )]);
    let at = case.at("calls.cpp", "recursive(input)");
    let result = case.trace(at.clone(), vec![at], 4);
    // Known extraction defect, not a semantic pass: this source contains a call.
    // The read-only query must expose missing indexed input instead of fabricating it.
    assert!(
        result.paths.is_empty()
            && result
                .gaps
                .iter()
                .any(|g| g.1 == "value_call_context_unavailable"),
        "{result:?}"
    );
}

#[test]
fn dereference_results_are_not_their_address_operands() {
    for (source, expected, operand) in [
        (
            "int read(int* address) { int value = *address; return value; }\n",
            "*address",
            "address",
        ),
        (
            "int read(const void* address) { int value = *reinterpret_cast<const int*>(address); return value; }\n",
            "*reinterpret_cast<const int*>(address)",
            "reinterpret_cast<const int*>(address)",
        ),
        (
            "struct Handle { int operator*(); }; int read(Handle address) { int value = *address; return value; }\n",
            "*address",
            "address",
        ),
    ] {
        let result = run(source, "value", 30);
        assert!(
            result.paths.iter().all(|p| p.endpoint.kind == "dereference"
                && p.endpoint.name.as_deref() == Some(expected)),
            "{result:#?}"
        );
        let gap = result
            .gaps
            .iter()
            .find(|g| g.1 == "value_dereference_unestablished")
            .expect("located dereference boundary");
        assert!(
            gap.3.iter().any(|at| &source
                [at.range.start_byte as usize..at.range.end_byte as usize]
                == operand),
            "{gap:#?}"
        );
    }
    for (source, selected) in [
        ("int* read(int* address) { return address; }\n", "address"),
        (
            "int* read(int* address) { return &*address; }\n",
            "&*address",
        ),
        (
            "int read(int left, int right) { int value = left * right; return value; }\n",
            "value",
        ),
    ] {
        let result = run(source, selected, 30);
        assert!(!result.paths.is_empty(), "{result:#?}");
        assert!(
            !result
                .gaps
                .iter()
                .any(|g| g.1 == "value_dereference_unestablished"),
            "{result:#?}"
        );
    }
    let case = CallsCase::new(&[(
        "calls.cpp",
        "int* address(int* input) { return input; } int read(int* input) { int value = *address(input); return value; }\n",
    )]);
    let result = case.trace(case.at("calls.cpp", "value"), vec![], 4);
    assert!(
        result
            .paths
            .iter()
            .all(|p| p.endpoint.kind == "dereference"),
        "{result:#?}"
    );
    assert_eq!(
        result.scopes.len(),
        1,
        "address-call bodies are separate investigation: {result:#?}"
    );
    let argument = CallsCase::new(&[(
        "args.cpp",
        "int pass(int input) { return input; } int read(int* address) { int value = pass(*address); return value; }\n",
    )]);
    // The source resolver has no applicable target for this dereference-typed
    // argument yet. Inspect its value role directly without inventing a call.
    let unresolved = argument.trace(argument.at("args.cpp", "value"), vec![], 4);
    assert!(
        unresolved
            .gaps
            .iter()
            .any(|g| g.1 == "value_call_target_unavailable")
    );
    let value = argument.trace(argument.at("args.cpp", "*address"), vec![], 4);
    assert!(
        value
            .paths
            .iter()
            .any(|p| p.endpoint.kind == "dereference"
                && p.endpoint.name.as_deref() == Some("*address")),
        "{value:#?}"
    );
    let address = case.trace(case.at("calls.cpp", "address(input)"), vec![], 4);
    assert!(
        address
            .paths
            .iter()
            .any(|p| p.endpoint.name.as_deref() == Some("input")),
        "{address:#?}"
    );
}
