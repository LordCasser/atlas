use super::*;
use crate::{ExtractionMode, IndexPipeline, IndexPipelineOptions, NoopSink};

mod navigation;
mod reuse;

fn fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, Arc<Store>) {
    let root = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let file = root.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        root.path().to_path_buf(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    (root, store)
}

fn inspect(root: &Path, store: &Store, source: &str, expression: &str) -> CallContextResult {
    let start = source.find(expression).unwrap() as u32;
    inspect_call_context(
        store,
        root,
        "main.cpp",
        start,
        start + expression.len() as u32,
        false,
        &|| false,
    )
    .unwrap()
}

fn item_text(root: &Path, store: &Store, item: &CallContextItem) -> String {
    let file = store.get_file(&item.location.file_id).unwrap().unwrap();
    let source = std::fs::read_to_string(root.join(file.path)).unwrap();
    source[item.location.range.start_byte as usize..item.location.range.end_byte as usize]
        .to_owned()
}

#[test]
fn operation_regions_preserve_calls_and_nested_body_ownership() {
    let source = "int value() { return 1; }\nvoid run(Bag &bag, int n) {\n    for (auto item : bag) { value(); }\n    n += value();\n    bag[n] = n + 1;\n    auto pending = [q = n + 2]() { return q + 3; };\n}\nint sibling() { return 9 * 4; }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let symbols = store.find_symbols_by_qname("run").unwrap();
    let run = symbols.iter().find(|s| s.range != s.name_range).unwrap();
    let before = store.find_references_by_file(&run.file_id).unwrap();
    let result = inspect_call_context(
        &store,
        root.path(),
        "main.cpp",
        run.range.start_byte,
        run.range.end_byte,
        false,
        &|| false,
    )
    .unwrap();
    let regions: Vec<_> = result
        .gaps
        .iter()
        .filter(|g| g.code == "cpp_operation_invocation_unexamined")
        .collect();
    let text = |g: &CallContextGap| {
        let at = g.subject.location().range;
        &source[at.start_byte as usize..at.end_byte as usize]
    };
    for expected in [
        "for (auto item : bag)",
        "n += value()",
        "bag[n] = n + 1",
        "bag[n]",
        "n + 1",
        "n + 2",
        "q + 3",
    ] {
        assert!(
            regions.iter().any(|g| text(g) == expected),
            "{expected}: {regions:#?}"
        );
    }
    assert!(
        !regions
            .iter()
            .any(|g| matches!(text(g), "9 * 4" | "value()"))
    );
    for gap in &regions {
        let ContextSubject::Region {
            symbol_id: Some(owner),
            ..
        } = gap.subject
        else {
            panic!("{gap:#?}")
        };
        if text(gap) == "q + 3" {
            assert_ne!(owner, run.id);
            assert!(
                store
                    .find_symbol_by_id(&owner)
                    .unwrap()
                    .unwrap()
                    .name
                    .starts_with("<lambda@")
            );
        } else {
            assert_eq!(owner, run.id, "{gap:#?}");
        }
    }
    let after = store.find_references_by_file(&run.file_id).unwrap();
    assert_eq!(before.len(), after.len());
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(before.id, after.id);
        assert_eq!(
            before.resolved.as_ref().map(|r| r.symbol_id),
            after.resolved.as_ref().map(|r| r.symbol_id)
        );
    }
    assert_eq!(
        after
            .iter()
            .filter(|r| r.name == "value" && r.kind == ReferenceKind::Call && r.resolved.is_some())
            .count(),
        2
    );
}

#[test]
fn operation_regions_distinguish_unevaluated_text_and_independent_closure_bodies() {
    let source = "void run(int n) {\n auto a = sizeof(n + 1);\n using T = decltype(n + 2);\n bool b = noexcept(n + 3);\n bool c = requires { n += 4; };\n const char* text = \"n + 5\";\n // n + 6\n auto d = sizeof([q = n + 7]() { return q + 8; });\n}\nint constant() { return 1 + 2; }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let selected = &source[..source.find("int constant").unwrap()];
    let result = inspect(root.path(), &store, source, selected);
    let regions: Vec<_> = result
        .gaps
        .iter()
        .filter(|g| g.code == "cpp_operation_invocation_unexamined")
        .collect();
    assert_eq!(regions.len(), 1, "{regions:#?}");
    let at = regions[0].subject.location().range;
    assert_eq!(
        &source[at.start_byte as usize..at.end_byte as usize],
        "q + 8"
    );
    let result = inspect(root.path(), &store, source, "1 + 2");
    let gap = result
        .gaps
        .iter()
        .find(|g| g.code == "cpp_operation_invocation_unexamined")
        .unwrap();
    assert!(gap.message.contains("built-in operations only"));
    // The diagnostic does not invent an invocation for built-in arithmetic.
    let file = store
        .find_files_by_path_prefix("main.cpp")
        .unwrap()
        .remove(0);
    assert!(
        !store
            .find_references_by_file(&file.file_id)
            .unwrap()
            .iter()
            .any(|r| r.name == "operator+")
    );
    assert!(
        inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            0,
            source.len() as u32,
            false,
            &|| true
        )
        .is_err()
    );
}

#[test]
fn preprocessing_navigation_preserves_alternative_headers_without_evaluating_them() {
    let source = "#ifndef OUTER\n#define OUTER\n#if FIRST\nusing Choice = int;\n#elif defined(SECOND) \\\n && MORE\n#if INNER\nusing Choice = long;\n#else\nusing Choice = char;\n#endif\n#else\nusing Choice = double;\n#endif\n#endif\nusing Final = bool;\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    for (selection, expected) in [
        ("int", vec!["#ifndef OUTER", "#if FIRST"]),
        (
            "char",
            vec![
                "#ifndef OUTER",
                "#if FIRST",
                "#elif defined(SECOND) \\\n && MORE",
                "#if INNER",
                "#else",
            ],
        ),
        (
            "double",
            vec![
                "#ifndef OUTER",
                "#if FIRST",
                "#elif defined(SECOND) \\\n && MORE",
                "#else",
            ],
        ),
        ("bool", vec![]),
    ] {
        let result = inspect(root.path(), &store, source, selection);
        let directives: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "preprocessing_directive")
            .collect();
        assert_eq!(
            directives
                .iter()
                .map(|i| item_text(root.path(), &store, i))
                .collect::<Vec<_>>(),
            expected,
            "{selection}: {result:#?}"
        );
        assert!(
            directives.iter().all(
                |i| i.symbol_id.is_none() && matches!(i.subject, ContextSubject::Region { .. })
            )
        );
        assert_eq!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "preprocessing_configuration_unestablished"),
            !expected.is_empty()
        );
    }
    let broad = inspect(
        root.path(),
        &store,
        source,
        "using Choice = long;\n#else\nusing Choice = char;",
    );
    let directives: Vec<_> = broad
        .items
        .iter()
        .filter(|i| i.role == "preprocessing_directive")
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert_eq!(
        directives,
        [
            "#ifndef OUTER",
            "#if FIRST",
            "#elif defined(SECOND) \\\n && MORE"
        ],
        "A range crossing alternatives retains only enclosing branch context"
    );
}

#[test]
fn preprocessing_navigation_ignores_text_in_literals_and_keeps_outer_context_on_body_errors() {
    let source = "const char* note = R\"note(\n#if GHOST\n)note\";\n/*\n#ifdef COMMENT\n*/\n#if ENABLED\nvoid broken() { bad @; }\n#endif\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    for selection in ["GHOST", "COMMENT"] {
        let result = inspect(root.path(), &store, source, selection);
        assert!(
            result
                .items
                .iter()
                .all(|i| i.role != "preprocessing_directive"),
            "{result:#?}"
        );
    }
    let result = inspect(root.path(), &store, source, "bad @");
    let directives: Vec<_> = result
        .items
        .iter()
        .filter(|i| i.role == "preprocessing_directive")
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert_eq!(directives, ["#if ENABLED"]);
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.code == "value_syntax_unexamined")
    );
    let incomplete = result
        .gaps
        .iter()
        .find(|g| g.code == "preprocessing_context_incomplete")
        .unwrap();
    assert!(
        incomplete.related_locations.iter().any(|at| {
            let source = std::fs::read_to_string(root.path().join("main.cpp")).unwrap();
            source[at.range.start_byte as usize..at.range.end_byte as usize].contains('@')
        }),
        "Recovery must have a concrete source continuation: {incomplete:#?}"
    );
}

#[test]
fn preprocessing_navigation_keeps_recorded_calls_and_literal_inactive_source() {
    let source = "void target(int value) {}\nvoid run() {\n#if 0\ntarget(1);\n#endif\n}\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let result = inspect(root.path(), &store, source, "target(1)");
    assert!(result.items.iter().any(
        |i| i.role == "preprocessing_directive" && item_text(root.path(), &store, i) == "#if 0"
    ));
    assert!(result.items.iter().all(|i| !i.role.starts_with("control_")));
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn argument_declared_types_keep_all_name_candidates_without_receiver_member_selection() {
    let source = "using Count = unsigned int; namespace other { using Count = long; } template<class T> struct Holder {}; struct Payload { void missing(); }; using Callback = int (*)(int); void run(Count& count, Holder<Payload>& holder, Callback callback) { missing(count); missing(holder); missing(callback); }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let count = inspect(root.path(), &store, source, "missing(count)");
    let references: Vec<_> = count
        .items
        .iter()
        .filter(|i| i.role == "type_reference")
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert_eq!(references, ["Count"]);
    let candidates: Vec<_> = count
        .items
        .iter()
        .filter(|i| i.role == "type_declaration")
        .map(|i| {
            store
                .find_symbol_by_id(&i.symbol_id.unwrap())
                .unwrap()
                .unwrap()
                .qualified_name
        })
        .collect();
    assert_eq!(candidates.len(), 2, "{count:#?}");
    assert!(candidates.contains(&"Count".into()) && candidates.contains(&"other::Count".into()));
    let mut targets: Vec<_> = count
        .items
        .iter()
        .filter(|i| i.role == "type_alias_target")
        .map(|i| {
            assert!(i.symbol_id.is_none());
            assert_eq!(i.related_locations.len(), 2);
            item_text(root.path(), &store, i)
        })
        .collect();
    targets.sort();
    assert_eq!(targets, ["long", "unsigned int"]);
    let holder = inspect(root.path(), &store, source, "missing(holder)");
    let references: Vec<_> = holder
        .items
        .iter()
        .filter(|i| i.role == "type_reference")
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert_eq!(references, ["Holder", "Payload"]);
    assert!(holder.items.iter().all(|i| i.role != "type_member"));
    assert!(
        holder
            .gaps
            .iter()
            .any(|g| g.code == "argument_value_unestablished")
    );
    let callback = inspect(root.path(), &store, source, "missing(callback)");
    assert!(callback.items.iter().any(|i| i.role == "type_declaration"));
    assert!(
        callback.items.iter().all(|i| i.role != "type_alias_target"),
        "An unsupported pointer alias must not invent a transparent target: {callback:#?}"
    );
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn argument_type_navigation_respects_shadowing_and_retains_missing_declarations() {
    let source = "struct Payload {}; void run(Payload& value, Missing& unknown) { { int value = 0; missing(value); } missing(unknown); }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let shadow = inspect(root.path(), &store, source, "missing(value)");
    assert!(
        shadow.items.iter().all(|i| !i.role.starts_with("type_")),
        "{shadow:#?}"
    );
    let missing = inspect(root.path(), &store, source, "missing(unknown)");
    let reference = missing
        .items
        .iter()
        .find(|i| i.role == "type_reference")
        .unwrap();
    assert_eq!(item_text(root.path(), &store, reference), "Missing");
    assert!(missing.items.iter().all(|i| i.role != "type_declaration"));
    assert!(
        missing
            .gaps
            .iter()
            .any(|g| g.code == "type_declaration_unavailable"
                && g.related_locations == [reference.location.clone()])
    );
}

#[test]
fn grammar_primitive_labels_do_not_hide_named_types_or_invent_keyword_declarations() {
    let source = "typedef unsigned int uint32_t; typedef unsigned long size_t; using nullptr_t = decltype(nullptr); void run(uint32_t& number, size_t length, nullptr_t null, char32_t character, bool flag, double fraction) { missing(number, length, null, character, flag, fraction); }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let result = inspect(
        root.path(),
        &store,
        source,
        "missing(number, length, null, character, flag, fraction)",
    );
    let references: Vec<_> = result
        .items
        .iter()
        .filter(|i| i.role == "type_reference")
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert_eq!(references, ["uint32_t", "size_t", "nullptr_t"]);
    let declarations: Vec<_> = result
        .items
        .iter()
        .filter(|i| i.role == "type_declaration")
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert_eq!(declarations, ["uint32_t", "size_t", "nullptr_t"]);
}

#[test]
fn template_callee_navigation_keeps_nested_calls_and_written_argument_forms_separate() {
    let source = r#"template<class T = int> void select() {}
template<int N> void count() {}
template<class T> auto factory() { return []{}; }
struct Tool { template<class T> void select() {} };
template<class Object> void invoke(Object* object) { object->template select<int>(); }
void run() { select<>(); (select<long>)(); count<7>(); factory<int>()(); }
"#;
    let (root, store) = fixture(&[("main.cpp", source)]);
    for (expression, name, args) in [
        ("select<>()", "select", "<>"),
        ("(select<long>)()", "select", "<long>"),
        ("count<7>()", "count", "<7>"),
        ("object->template select<int>()", "select", "<int>"),
    ] {
        let result = inspect(root.path(), &store, source, expression);
        let names: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "callable_name")
            .map(|i| item_text(root.path(), &store, i))
            .collect();
        let arguments: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "template_arguments")
            .map(|i| item_text(root.path(), &store, i))
            .collect();
        assert_eq!(names, [name], "{expression}: {result:#?}");
        assert_eq!(arguments, [args], "{expression}: {result:#?}");
    }
    let nested = inspect(root.path(), &store, source, "factory<int>()()");
    let names: Vec<_> = nested
        .items
        .iter()
        .filter(|i| i.role == "callable_name")
        .collect();
    assert_eq!(names.len(), 1, "{nested:#?}");
    assert_eq!(names[0].subject.call().unwrap().text, "factory<int>");
    assert!(
        inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            0,
            source.len() as u32,
            false,
            &|| true
        )
        .is_err()
    );
}

#[test]
fn conversions_and_comparisons_are_not_named_template_call_targets() {
    let source = "void sink(bool); void run(int* first, int* last) { const int* value = static_cast<const int*>(first); sink(value < last); }";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let result = inspect(
        root.path(),
        &store,
        source,
        "const int* value = static_cast<const int*>(first); sink(value < last);",
    );
    assert!(
        !result
            .items
            .iter()
            .any(|i| matches!(i.role, "callable_name" | "template_arguments")),
        "{result:#?}"
    );
    assert!(
        !result
            .gaps
            .iter()
            .any(|g| g.code == "template_call_target_unestablished")
    );
}

#[test]
fn explicit_template_calls_keep_names_arguments_and_declaration_candidates() {
    let source = "template<class T> bool choose(T& value) { return true; } namespace other { template<class T> bool choose(T& value) { return false; } } struct Box { template<class T> bool choose(T& value) { return true; } }; void run(Box& box, int& value) { choose<int>(value); other::choose<long>(value); box.choose<int>(value); }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    for expression in [
        "choose<int>(value)",
        "other::choose<long>(value)",
        "box.choose<int>(value)",
    ] {
        let result = inspect(root.path(), &store, source, expression);
        let names: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "callable_name")
            .collect();
        assert_eq!(names.len(), 1, "{expression}: {result:#?}");
        assert_eq!(item_text(root.path(), &store, names[0]), "choose");
        let arguments: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "template_arguments")
            .collect();
        assert_eq!(arguments.len(), 1, "{result:#?}");
        assert!(item_text(root.path(), &store, arguments[0]).starts_with('<'));
        let candidates: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "same_name")
            .collect();
        assert!(
            candidates.len() >= 3,
            "candidates from other scopes must not be silently excluded: {result:#?}"
        );
        assert!(
            candidates
                .iter()
                .all(|i| i.symbol_id.is_some() && !i.related_locations.is_empty())
        );
        assert_eq!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "template_call_target_unestablished"),
            expression.starts_with("other::"),
            "Explicit int calls now resolve; long& cannot bind the int argument: {result:#?}"
        );
    }
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn noncall_values_navigate_fields_and_local_inputs_without_persisted_uses() {
    let source = r#"namespace selected {
struct Envelope { int stored_; int read() const { return stored_; } };
void save(Envelope* destination, int input) { destination->stored_ = input; }
}
namespace unrelated { struct Envelope { int stored_; }; }
"#;
    // Retained Ready Artifacts can have declarations/scopes without persisted
    // BindingUses. Seed that state explicitly; the normal fixture keeps uses.
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.cpp"), source).unwrap();
    let mut facts = extraction::extract_file_with_mode(
        &extraction::create_frontend(Language::Cpp).unwrap(),
        FileId::generate("main.cpp"),
        Path::new("main.cpp"),
        source,
        "value-context",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    facts.binding_uses.clear();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    store.insert_file_facts(&facts).unwrap();
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    assert!(
        store
            .find_binding_uses_by_file(&FileId::generate("main.cpp"))
            .unwrap()
            .is_empty()
    );
    let getter = inspect(root.path(), &store, source, "return stored_;");
    let fields: Vec<_> = getter
        .items
        .iter()
        .filter(|i| i.role == "value_binding")
        .collect();
    assert_eq!(fields.len(), 1, "{getter:#?}");
    assert_eq!(item_text(root.path(), &store, fields[0]), "int stored_;");
    assert_eq!(
        store
            .find_symbol_by_id(&fields[0].symbol_id.unwrap())
            .unwrap()
            .unwrap()
            .qualified_name,
        "selected::Envelope::stored_"
    );
    let saved = inspect(root.path(), &store, source, "destination->stored_ = input");
    let declarations: Vec<_> = saved
        .items
        .iter()
        .filter(|i| matches!(i.role, "value_binding" | "receiver_binding"))
        .map(|i| item_text(root.path(), &store, i))
        .collect();
    assert!(
        declarations.contains(&"Envelope* destination".into()),
        "{saved:#?}"
    );
    assert!(declarations.contains(&"int input".into()), "{saved:#?}");
    for result in [&getter, &saved] {
        assert!(
            result
                .items
                .iter()
                .all(|i| matches!(i.subject, ContextSubject::Region { .. }))
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "value_context_limited")
        );
        assert_eq!(result.files_read, 1);
    }
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
    assert!(
        store
            .find_binding_uses_by_file(&FileId::generate("main.cpp"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn noncall_values_preserve_shadowing_explicit_this_and_separate_use_positions() {
    let source = "struct Box { int field; int shadow(int field) { return field; } int explicit_member(int field) { return this->field; } int repeated() { return field + field; } };";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let shadow = inspect(
        root.path(),
        &store,
        source,
        "int shadow(int field) { return field; }",
    );
    let items: Vec<_> = shadow
        .items
        .iter()
        .filter(|i| i.role == "value_binding")
        .collect();
    assert_eq!(items.len(), 1, "{shadow:#?}");
    assert_eq!(item_text(root.path(), &store, items[0]), "int field");
    let explicit = inspect(root.path(), &store, source, "return this->field;");
    let item = explicit
        .items
        .iter()
        .find(|i| i.role == "value_binding")
        .unwrap();
    assert_eq!(item_text(root.path(), &store, item), "int field;");
    let repeated = inspect(root.path(), &store, source, "return field + field;");
    let items: Vec<_> = repeated
        .items
        .iter()
        .filter(|i| i.role == "value_binding")
        .collect();
    assert_eq!(items.len(), 2, "{repeated:#?}");
    assert_eq!(items[0].symbol_id, items[1].symbol_id);
    assert_ne!(items[0].subject.location(), items[1].subject.location());
}

#[test]
fn noncall_unknown_object_and_source_failure_do_not_borrow_field_identity() {
    let source = "struct Box { int field; }; int elsewhere() { return field; }";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let unknown = inspect(root.path(), &store, source, "return field;");
    assert!(!unknown.items.iter().any(|i| i.role == "value_binding"));
    assert!(
        unknown
            .gaps
            .iter()
            .any(|g| g.code == "receiver_binding_unavailable")
    );
    std::fs::remove_file(root.path().join("main.cpp")).unwrap();
    let failed = inspect(root.path(), &store, source, "return field;");
    assert!(failed.items.is_empty());
    assert!(
        failed
            .gaps
            .iter()
            .any(|g| g.code == "source_context_unavailable"
                && matches!(g.subject, ContextSubject::Region { .. }))
    );
    assert!(
        !failed
            .gaps
            .iter()
            .any(|g| g.code == "value_context_limited")
    );
}

#[test]
fn unowned_calls_keep_argument_and_callback_navigation_without_inventing_edges() {
    let source = r#"namespace project {
int leaf(int value) { return value; }
void enqueue(int (*callback)(int), int value);
using Callback = int (*)(int);
Callback factory(int value);
template<class T> struct Box { void run(int input); };
template<class T> void Box<T>::run(int input) {
    auto task = [](int number) { return leaf(number); };
    enqueue(task, input);
    enqueue(leaf, input);
    factory(input)(7);
}
int entry() { return leaf(1); }
}"#;
    let (root, store) = fixture(&[("main.cpp", source)]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let result = inspect(root.path(), &store, source, "enqueue(task, input)");
    let arguments: Vec<_> = result
        .items
        .iter()
        .filter(|item| item.role == "argument_expression")
        .collect();
    assert_eq!(
        arguments
            .iter()
            .map(|item| item_text(root.path(), &store, item))
            .collect::<Vec<_>>(),
        ["task", "input"],
        "{result:#?}"
    );
    assert!(
        result
            .items
            .iter()
            .all(|item| item.subject.call().unwrap().source_symbol.is_none())
    );
    assert!(
        result
            .items
            .iter()
            .all(|item| item.role != "argument_parameter")
    );
    let origins: Vec<_> = result
        .items
        .iter()
        .filter(|item| item.role == "argument_binding")
        .map(|item| item_text(root.path(), &store, item))
        .collect();
    assert_eq!(
        origins,
        [
            "auto task = [](int number) { return leaf(number); };",
            "int input"
        ]
    );
    let callback = result
        .items
        .iter()
        .find(|item| item.role == "argument_initializer")
        .unwrap();
    assert_eq!(
        item_text(root.path(), &store, callback),
        "[](int number) { return leaf(number); }"
    );
    let id = callback
        .symbol_id
        .expect("the independently indexed callback remains navigable");
    let body_call = store
        .find_references_by_file(&FileId::generate("main.cpp"))
        .unwrap()
        .into_iter()
        .find(|reference| reference.range.start_byte == source.find("leaf(number)").unwrap() as u32)
        .unwrap();
    assert_eq!(body_call.source_symbol, Some(id), "{body_call:#?}");
    assert!(
        !result
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_locations_unavailable")
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_value_unestablished")
    );

    let function_use = inspect(root.path(), &store, source, "enqueue(leaf, input)");
    assert!(
        function_use
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_binding_unavailable")
    );
    assert!(
        !function_use
            .items
            .iter()
            .any(|item| item.role == "argument_binding"
                && item_text(root.path(), &store, item).contains("leaf"))
    );
    let nested = inspect(root.path(), &store, source, "factory(input)(7)");
    for (callee, expected) in [("factory", "input"), ("factory(input)", "7")] {
        let args: Vec<_> = nested
            .items
            .iter()
            .filter(|item| {
                item.role == "argument_expression" && item.subject.call().unwrap().text == callee
            })
            .collect();
        assert_eq!(args.len(), 1, "{callee}: {nested:#?}");
        assert_eq!(item_text(root.path(), &store, args[0]), expected);
    }
    assert_eq!(
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap(),
        before
    );
}

#[test]
fn argument_positions_use_the_selected_definition_not_same_name_declarations() {
    let header = "namespace chosen { int transfer(int declaration_name, int optional = 3); } namespace decoy { int transfer(int unrelated, int second); } void unavailable(int); void overloaded(int); void overloaded(char); void ambiguous(int*); void ambiguous(char*);";
    let definition = "#include \"api.hpp\"\nnamespace chosen { int transfer(int body_value, int offset) { return body_value + offset; } }";
    let source = "#include \"api.hpp\"\nvoid entry(int input) { chosen::transfer(input, 7); chosen::transfer(input); unavailable(input); overloaded(input); ambiguous(nullptr); absent(input); }";
    let (root, store) = fixture(&[
        ("main.cpp", source),
        ("api.hpp", header),
        ("impl.cpp", definition),
    ]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let full = inspect(root.path(), &store, source, "chosen::transfer(input, 7)");
    let mappings: Vec<_> = full
        .items
        .iter()
        .filter(|item| item.role == "argument_parameter")
        .collect();
    assert_eq!(mappings.len(), 2, "{full:#?}");
    assert_eq!(
        mappings
            .iter()
            .map(|item| item_text(root.path(), &store, item))
            .collect::<Vec<_>>(),
        ["int body_value", "int offset"]
    );
    for (item, argument) in mappings.iter().zip(["input", "7"]) {
        assert_eq!(
            store
                .get_file(&item.location.file_id)
                .unwrap()
                .unwrap()
                .path,
            "impl.cpp"
        );
        let actual = &item.related_locations[0];
        assert_eq!(
            &source[actual.range.start_byte as usize..actual.range.end_byte as usize],
            argument
        );
        let callee = store
            .find_symbol_by_id(&item.symbol_id.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(callee.qualified_name, "chosen::transfer");
        assert_eq!(
            store.get_file(&callee.file_id).unwrap().unwrap().path,
            "impl.cpp"
        );
    }
    let defaulted = inspect(root.path(), &store, source, "chosen::transfer(input)");
    assert_eq!(
        defaulted
            .items
            .iter()
            .filter(|item| item.role == "argument_parameter")
            .count(),
        1,
        "{defaulted:#?}"
    );
    let declaration = inspect(root.path(), &store, source, "unavailable(input)");
    let parameter = declaration
        .items
        .iter()
        .find(|item| item.role == "argument_parameter")
        .unwrap();
    assert_eq!(item_text(root.path(), &store, parameter), "int");
    assert_eq!(
        store
            .get_file(&parameter.location.file_id)
            .unwrap()
            .unwrap()
            .path,
        "api.hpp"
    );
    let overload = inspect(root.path(), &store, source, "overloaded(input)");
    let parameter = overload
        .items
        .iter()
        .find(|item| item.role == "argument_parameter")
        .unwrap();
    assert_eq!(item_text(root.path(), &store, parameter), "int");
    for expression in ["ambiguous(nullptr)", "absent(input)"] {
        let result = inspect(root.path(), &store, source, expression);
        assert!(
            result
                .items
                .iter()
                .all(|item| item.role != "argument_parameter"),
            "{expression}: {result:#?}"
        );
        assert!(
            result
                .items
                .iter()
                .any(|item| item.role == "argument_expression")
        );
    }
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn arguments_navigate_written_origins_without_claiming_values_or_execution() {
    let source = r#"
#include <functional>
void first() {} void second() {}
void entry() {
    std::function<void()> task = [] { first(); };
    task = [] { second(); };
    enqueue(task);
    { auto task = [] { second(); }; shadow(task); }
    direct([] { first(); });
    compound(task, wrap(task), 7);
    auto captured = [&task] { captured_use(task); };
    auto missing = [] { missing_capture(task); };
    auto replaced = [task = 7] { init_capture(task); };
}
void forward(std::function<void()> task) { forward_use(task); }
void partial() {
    auto task = [] { LOG("value:%" UNKNOWN_FORMAT, 7); first(); };
    partial_use(task);
}
void other() { unavailable(task); }
void ambiguous() { int task = 1; int task = 2; ambiguous_use(task); }
"#;
    let (root, store) = fixture(&[("main.cpp", source)]);
    let before = serde_json::to_value(store.get_all_call_references().unwrap()).unwrap();
    let edges = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let reassigned = inspect(root.path(), &store, source, "enqueue(task)");
    let initializer = reassigned
        .items
        .iter()
        .find(|item| item.role == "argument_initializer")
        .unwrap();
    assert_eq!(
        item_text(root.path(), &store, initializer),
        "[] { first(); }"
    );
    let callable = store
        .find_symbol_by_id(&initializer.symbol_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(callable.range, initializer.location.range);
    assert!(initializer.message.contains("Later assignments"));
    assert!(
        reassigned
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_value_unestablished")
    );

    let shadow = inspect(root.path(), &store, source, "shadow(task)");
    let origins: Vec<_> = shadow
        .items
        .iter()
        .filter(|item| item.role == "argument_initializer")
        .map(|item| item_text(root.path(), &store, item))
        .collect();
    assert_eq!(origins, ["[] { second(); }"]);
    let direct = inspect(root.path(), &store, source, "direct([] { first(); })");
    assert!(
        direct
            .items
            .iter()
            .any(|item| item.role == "argument_expression" && item.symbol_id.is_some())
    );
    let compound = inspect(root.path(), &store, source, "compound(task, wrap(task), 7)");
    let expressions: Vec<_> = compound
        .items
        .iter()
        .filter(|item| {
            item.subject
                .call()
                .is_some_and(|call| call.name == "compound")
                && item.role == "argument_expression"
        })
        .map(|item| item_text(root.path(), &store, item))
        .collect();
    assert_eq!(expressions, ["task", "wrap(task)", "7"]);
    // An inner call has its own subject, not the outer call's argument identity.
    assert!(compound.items.iter().any(|item| {
        item.subject.call().is_some_and(|call| call.name == "wrap")
            && item.role == "argument_binding"
    }));
    let captured = inspect(root.path(), &store, source, "captured_use(task)");
    assert!(
        captured
            .items
            .iter()
            .any(|item| item.role == "argument_binding")
    );
    for expression in ["missing_capture(task)", "init_capture(task)"] {
        let result = inspect(root.path(), &store, source, expression);
        assert!(
            result
                .items
                .iter()
                .all(|item| item.role != "argument_binding"),
            "{expression}: {result:#?}"
        );
        assert!(
            result.gaps.iter().any(|gap| matches!(
                gap.code,
                "argument_capture_unverified" | "argument_binding_unavailable"
            )),
            "{expression}: {result:#?}"
        );
    }
    let forward = inspect(root.path(), &store, source, "forward_use(task)");
    assert!(
        forward
            .items
            .iter()
            .any(|item| item.role == "argument_binding"
                && item_text(root.path(), &store, item) == "std::function<void()> task")
    );
    assert!(
        forward
            .items
            .iter()
            .all(|item| item.role != "argument_initializer")
    );
    let partial = inspect(root.path(), &store, source, "partial_use(task)");
    assert!(
        partial
            .items
            .iter()
            .any(|item| item.role == "argument_initializer" && item.symbol_id.is_some()),
        "{partial:#?}"
    );
    assert!(
        partial
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_declaration_syntax_partial"),
        "{partial:#?}"
    );
    let unavailable = inspect(root.path(), &store, source, "unavailable(task)");
    assert!(
        unavailable
            .items
            .iter()
            .all(|item| item.role != "argument_binding")
    );
    assert!(
        unavailable
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_binding_unavailable")
    );
    let ambiguous = inspect(root.path(), &store, source, "ambiguous_use(task)");
    assert_eq!(
        ambiguous
            .items
            .iter()
            .filter(|item| item.role == "argument_binding")
            .count(),
        2
    );
    assert!(
        ambiguous
            .gaps
            .iter()
            .any(|gap| gap.code == "argument_binding_ambiguous")
    );
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_call_references().unwrap()).unwrap()
    );
    assert_eq!(
        edges,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn candidate_type_recovery_locations_remain_local_and_do_not_select_a_receiver() {
    let header = r#"
namespace damaged {
class Device {
public:
    auto callback() { return []() UNKNOWN(guard) -> int { return 1; }; }
    void run() {}
};
}
namespace clean { class Device { public: void run() {} }; }
"#;
    let source = "#include \"api.hpp\"\nvoid entry(damaged::Device* receiver) { receiver->run(); }";
    let (root, store) = fixture(&[("main.cpp", source), ("api.hpp", header)]);
    let before = serde_json::to_value(store.get_all_call_references().unwrap()).unwrap();
    let edges = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let result = inspect(root.path(), &store, source, "receiver->run()");
    let recoveries: Vec<_> = result
        .gaps
        .iter()
        .filter(|gap| gap.code == "candidate_type_syntax_recovery")
        .collect();
    assert!(!recoveries.is_empty(), "{result:#?}");
    let class_end = header.find("namespace clean").unwrap() as u32;
    for gap in recoveries {
        assert!(gap.message.contains("damaged::Device"), "{gap:#?}");
        assert_eq!(gap.related_locations.len(), 2);
        let location = &gap.related_locations[1];
        let file = store.get_file(&location.file_id).unwrap().unwrap();
        assert_eq!(file.path, "api.hpp");
        assert!(location.range.start_byte < class_end);
        assert!(location.range.end_byte <= class_end);
        assert!(location.range.end_byte >= location.range.start_byte);
    }
    let member_names: BTreeSet<_> = result
        .items
        .iter()
        .filter(|item| item.role == "type_member")
        .map(|item| {
            store
                .find_symbol_by_id(&item.symbol_id.unwrap())
                .unwrap()
                .unwrap()
                .qualified_name
        })
        .collect();
    assert!(member_names.contains("damaged::Device::run"));
    assert!(member_names.contains("clean::Device::run"));
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_call_references().unwrap()).unwrap()
    );
    assert_eq!(
        edges,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn initializer_return_and_type_context_work_without_wrapper_dependencies_or_graph_writes() {
    let header = "namespace demo { class Device { public: void run(); }; class Root { public: Missing<Device> Make(); void entry(); }; }";
    let source = "#include \"api.hpp\"\nnamespace demo { Missing<Device> Root::Make() { return {}; } void Root::entry() { auto receiver = Make(); receiver->run(); } void Device::run() {} }";
    let unrelated =
        "namespace other { class Device { public: void run(); }; void Device::run() {} }";
    let (root, store) = fixture(&[
        ("main.cpp", source),
        ("api.hpp", header),
        ("other.cpp", unrelated),
    ]);
    let before = serde_json::to_value(store.get_all_call_references().unwrap()).unwrap();
    let edges_before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let result = inspect(root.path(), &store, source, "receiver->run()");
    let roles: BTreeSet<_> = result.items.iter().map(|item| item.role).collect();
    for role in [
        "receiver_binding",
        "initializer",
        "callable_declaration",
        "type_reference",
        "type_declaration",
        "type_member",
    ] {
        assert!(roles.contains(role), "{role}: {result:#?}");
    }
    assert!(result.items.iter().any(|i| i.role == "receiver_binding"
        && item_text(root.path(), &store, i) == "auto receiver = Make();"));
    assert!(result.items.iter().any(|i| i.role == "callable_declaration"
        && item_text(root.path(), &store, i).contains("Missing<Device> Make()")));
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.code == "type_declaration_unavailable" && g.message.contains("Missing")),
        "{result:#?}"
    );
    assert!(
        result
            .items
            .iter()
            .filter(|i| !matches!(i.role, "selected_callable" | "enclosing_callable"))
            .all(|i| !i.related_locations.is_empty())
    );
    // Same-name types in another namespace are still hypotheses, never exclusions.
    let member_names: BTreeSet<_> = result
        .items
        .iter()
        .filter(|i| i.role == "type_member")
        .map(|i| {
            store
                .find_symbol_by_id(&i.symbol_id.unwrap())
                .unwrap()
                .unwrap()
                .qualified_name
        })
        .collect();
    assert!(member_names.contains("demo::Device::run"));
    assert!(member_names.contains("other::Device::run"));
    assert!(
        result.files_read < 3,
        "Only the call/factory source files need parsing: {result:#?}"
    );
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_call_references().unwrap()).unwrap()
    );
    assert_eq!(
        edges_before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn nearest_scope_and_ambiguous_bindings_retain_distinct_origins() {
    let source = r#"namespace demo {
class A { public: void run(); }; class B { public: void run(); };
A* MakeA(); B* MakeB();
void outer() {
    auto receiver = MakeA();
    { auto receiver = MakeB(); receiver->run(); }
    receiver->run();
}
void ambiguous() { auto value = MakeA(); auto value = MakeB(); value->run(); }
void alias() { auto receiver = another; receiver->run(); }
}"#;
    let (root, store) = fixture(&[("main.cpp", source)]);
    let first = source.find("receiver->run()").unwrap();
    let second = source[first + 1..].find("receiver->run()").unwrap() + first + 1;
    for (offset, wanted, excluded) in [
        (first, "MakeB()", "MakeA()"),
        (second, "MakeA()", "MakeB()"),
    ] {
        let result = inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            offset as u32,
            (offset + 15) as u32,
            false,
            &|| false,
        )
        .unwrap();
        let initializers: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "initializer")
            .map(|i| item_text(root.path(), &store, i))
            .collect();
        assert!(initializers.iter().any(|s| s == wanted), "{result:#?}");
        assert!(!initializers.iter().any(|s| s == excluded), "{result:#?}");
    }
    let ambiguous = inspect(root.path(), &store, source, "value->run()");
    assert!(
        ambiguous
            .gaps
            .iter()
            .any(|g| g.code == "receiver_binding_ambiguous"),
        "{ambiguous:#?}"
    );
    assert_eq!(
        ambiguous
            .items
            .iter()
            .filter(|i| i.role == "receiver_binding")
            .count(),
        2
    );
    let alias_start = source.rfind("receiver->run()").unwrap() as u32;
    let alias = inspect_call_context(
        &store,
        root.path(),
        "main.cpp",
        alias_start,
        alias_start + 15,
        false,
        &|| false,
    )
    .unwrap();
    assert!(
        alias
            .gaps
            .iter()
            .any(|g| g.code == "initializer_value_unestablished"),
        "{alias:#?}"
    );
    assert!(alias.items.iter().all(|i| i.role != "callable_declaration"));
}

#[test]
fn explicit_object_pointer_and_reference_declarations_are_available() {
    let source = "class Device { public: void run(); }; void example(Device& input) { Device local; local.run(); Device* pointer = &input; pointer->run(); Device& reference = input; reference.run(); }";
    let (root, store) = fixture(&[("main.cpp", source)]);
    for expression in ["local.run()", "pointer->run()", "reference.run()"] {
        let result = inspect(root.path(), &store, source, expression);
        assert!(
            result.items.iter().any(|i| i.role == "receiver_binding"),
            "{expression}: {result:#?}"
        );
        assert!(
            result.items.iter().any(|i| i.role == "type_member"),
            "{expression}: {result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "receiver_semantics_unverified")
        );
    }
}

#[test]
fn class_fields_supply_declaration_context_across_files_without_smart_pointer_inference() {
    let header = "namespace demo { class Device { public: void run(); }; class Owner { Missing<Device> device_; public: void entry(); }; }";
    let source = "#include \"api.hpp\"\nnamespace demo { void Owner::entry() { device_->run(); this->device_->run(); } }";
    let noise = "namespace other { class Device { public: void run(); }; class Owner { Missing<Device> device_; }; }";
    let (root, store) = fixture(&[
        ("main.cpp", source),
        ("api.hpp", header),
        ("noise.cpp", noise),
    ]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    for expression in ["device_->run()", "this->device_->run()"] {
        let result = inspect(root.path(), &store, source, expression);
        let fields: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.role == "receiver_binding")
            .collect();
        assert_eq!(fields.len(), 1, "{result:#?}");
        assert_eq!(
            item_text(root.path(), &store, fields[0]),
            "Missing<Device> device_;"
        );
        assert_eq!(
            store
                .find_symbol_by_id(&fields[0].symbol_id.unwrap())
                .unwrap()
                .unwrap()
                .qualified_name,
            "demo::Owner::device_"
        );
        assert!(
            result.items.iter().any(|i| i.role == "type_member"),
            "{result:#?}"
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "type_declaration_unavailable" && g.message.contains("Missing"))
        );
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "receiver_semantics_unverified")
        );
        assert!(
            !result
                .gaps
                .iter()
                .any(|g| g.code == "receiver_binding_unavailable")
        );
    }
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn lexical_shadowing_and_lambda_captures_do_not_become_class_fields() {
    let source = r#"
class A { public: void run(); }; class B { public: void run(); };
class Owner { A* device_; public: void entry(B* device_) {
    device_->run(); this->device_->run();
} void capture(B* other) { auto work = [device_ = other]() { device_->run(); }; }
};
"#;
    let (root, store) = fixture(&[("main.cpp", source)]);
    let local = inspect(root.path(), &store, source, "device_->run()");
    assert!(
        local
            .items
            .iter()
            .any(|i| i.role == "receiver_binding"
                && item_text(root.path(), &store, i) == "B* device_"),
        "{local:#?}"
    );
    let explicit = inspect(root.path(), &store, source, "this->device_->run()");
    assert!(
        explicit
            .items
            .iter()
            .any(|i| i.role == "receiver_binding"
                && item_text(root.path(), &store, i) == "A* device_;"),
        "{explicit:#?}"
    );
    let offset = source.rfind("device_->run()").unwrap() as u32;
    let captured = inspect_call_context(
        &store,
        root.path(),
        "main.cpp",
        offset,
        offset + 14,
        false,
        &|| false,
    )
    .unwrap();
    assert!(
        captured.items.iter().all(|i| i.role != "receiver_binding"),
        "{captured:#?}"
    );
    assert!(
        captured
            .gaps
            .iter()
            .any(|g| g.code == "receiver_capture_unverified")
    );
}

#[test]
fn repeated_class_definitions_remain_ambiguous_and_inherited_fields_are_not_guessed() {
    let source = "class Owner { public: void entry(); }; void Owner::entry() { device_->run(); } class Base { Device* inherited_; }; class Child : public Base { void entry() { inherited_->run(); } };";
    let (root, store) = fixture(&[
        ("main.cpp", source),
        ("variant_a.hpp", "class Owner { DeviceA* device_; };"),
        ("variant_b.hpp", "class Owner { DeviceB* device_; };"),
    ]);
    let result = inspect(root.path(), &store, source, "device_->run()");
    assert_eq!(
        result
            .items
            .iter()
            .filter(|i| i.role == "receiver_binding")
            .count(),
        2,
        "{result:#?}"
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.code == "receiver_binding_ambiguous")
    );
    let inherited = inspect(root.path(), &store, source, "inherited_->run()");
    assert!(inherited.items.iter().all(|i| i.role != "receiver_binding"));
    assert!(
        inherited
            .gaps
            .iter()
            .any(|g| g.code == "receiver_binding_unavailable")
    );
}

#[test]
fn source_failure_and_cancellation_do_not_report_context_completion() {
    let source = "void example() { auto value = Make(); value->run(); }";
    let (root, store) = fixture(&[("main.cpp", source)]);
    assert!(
        inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            0,
            source.len() as u32,
            false,
            &|| true
        )
        .is_err()
    );
    std::fs::remove_file(root.path().join("main.cpp")).unwrap();
    let result = inspect(root.path(), &store, source, "value->run()");
    assert!(result.items.is_empty());
    assert!(
        result
            .gaps
            .iter()
            .any(|g| g.code == "source_context_unavailable")
    );
}

#[test]
fn annotation_normalization_preserves_original_class_source_and_receiver_context() {
    let header = "#include \"attrs.hpp\"\nnamespace demo { class API Device final { public: int read() const { return 7; } }; }";
    let source =
        "#include \"device.hpp\"\nint entry(demo::Device* device) { return device->read(); }";
    let (root, store) = fixture(&[
        (
            "attrs.hpp",
            "#define API __attribute__((visibility(\"default\")))\n",
        ),
        ("device.hpp", header),
        ("main.cpp", source),
    ]);
    let before = serde_json::to_value(store.get_all_edges().unwrap()).unwrap();
    let result = inspect(root.path(), &store, source, "device->read()");
    assert!(
        result.items.iter().any(
            |i| i.role == "type_member" && item_text(root.path(), &store, i).contains("read()")
        ),
        "{result:#?}"
    );
    let symbols = store.find_symbols_by_qname("demo::Device").unwrap();
    assert_eq!(symbols.len(), 1);
    let original = crate::SourceExtractor::new(store.clone(), root.path().into())
        .extract_source(&symbols[0].id)
        .unwrap();
    assert_eq!(
        original,
        "class API Device final { public: int read() const { return 7; } }"
    );
    let methods = store.find_symbols_by_qname("demo::Device::read").unwrap();
    assert_eq!(methods.len(), 1);
    assert_eq!(
        crate::SourceExtractor::new(store.clone(), root.path().into())
            .extract_source(&methods[0].id)
            .unwrap(),
        "int read() const { return 7; }"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("device.hpp")).unwrap(),
        header
    );
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_edges().unwrap()).unwrap()
    );
}

#[test]
fn closure_receivers_share_recorded_capture_facts_with_resolution() {
    let source = r#"
struct Device { void run() {} };
struct Owner {
    Device field;
    void entry(Device* pointer) {
        auto by_this = [this] { field.run(); };
        auto by_pointer = [pointer] { pointer->run(); };
        static Device stored;
        auto by_static = [] { stored.run(); };
        auto missing = [] { field.run(); };
        auto missing_pointer = [] { pointer->run(); };
        auto shadow = [this, field = pointer] { field->run(); };
    }
};
"#;
    let (root, store) = fixture(&[("main.cpp", source)]);
    let before = serde_json::to_value(store.get_all_call_references().unwrap()).unwrap();
    for (start_marker, expression, declaration, capture_available) in [
        ("auto by_this", "field.run()", "Device field;", true),
        ("auto by_pointer", "pointer->run()", "Device* pointer", true),
        (
            "auto by_static",
            "stored.run()",
            "static Device stored;",
            true,
        ),
        ("auto missing =", "field.run()", "", false),
        ("auto missing_pointer", "pointer->run()", "", false),
        ("auto shadow", "field->run()", "", false),
    ] {
        let start = source.find(start_marker).unwrap();
        let offset = (start + source[start..].find(expression).unwrap()) as u32;
        let result = inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            offset,
            offset + expression.len() as u32,
            false,
            &|| false,
        )
        .unwrap();
        if capture_available {
            assert!(
                result
                    .items
                    .iter()
                    .any(|item| item.role == "receiver_binding"
                        && item_text(root.path(), &store, item) == declaration),
                "{start_marker}: {result:#?}"
            );
            assert!(
                !result
                    .gaps
                    .iter()
                    .any(|gap| gap.code == "receiver_capture_unverified"),
                "{start_marker}: {result:#?}"
            );
        } else {
            assert!(
                result
                    .items
                    .iter()
                    .all(|item| item.role != "receiver_binding"),
                "{start_marker}: {result:#?}"
            );
            assert!(
                result
                    .gaps
                    .iter()
                    .any(|gap| gap.code == "receiver_capture_unverified"),
                "{start_marker}: {result:#?}"
            );
            assert!(
                !result
                    .gaps
                    .iter()
                    .any(|gap| gap.code == "receiver_semantics_unverified"),
                "{start_marker}: {result:#?}"
            );
        }
    }
    assert_eq!(
        before,
        serde_json::to_value(store.get_all_call_references().unwrap()).unwrap()
    );
}

#[test]
fn namespace_receiver_context_limit_is_not_reported_as_missing_capture() {
    let source = "struct Device { void run() {} }; static Device device; void entry() { auto task = [] { device.run(); }; }";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let calls = store.get_all_call_references().unwrap();
    assert!(
        calls
            .iter()
            .any(|call| call.name == "run" && call.resolved.is_some())
    );
    let result = inspect(root.path(), &store, source, "device.run()");
    assert!(
        result
            .items
            .iter()
            .all(|item| item.role != "receiver_binding")
    );
    assert!(
        result
            .gaps
            .iter()
            .any(|gap| gap.code == "receiver_binding_unavailable")
    );
    assert!(
        !result
            .gaps
            .iter()
            .any(|gap| gap.code == "receiver_capture_unverified")
    );
}
