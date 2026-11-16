use db::Store;
use extraction::ExtractionMode;
use filesync::{IncrementalPipeline, IndexPipeline, IndexPipelineOptions, NoopSink};
use std::{path::Path, sync::Arc};

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
}
fn targets(store: &Store) -> Vec<String> {
    let entry = store.find_symbols_by_qname("entry").unwrap();
    assert_eq!(entry.len(), 1);
    store
        .find_edges_by_source(&entry[0].id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == types::EdgeKind::Calls)
        .map(|e| {
            store
                .find_symbol_by_id(&e.target)
                .unwrap()
                .unwrap()
                .qualified_name
        })
        .collect()
}

#[test]
fn added_attribute_wrapper_header_preserves_calls_and_revalidates_semantic_changes() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "attributes.hpp",
        "#include \"compat.hpp\"\n#define API __attribute__((visibility(\"hidden\")))\n",
    );
    let declaration = "#include \"attributes.hpp\"\nnamespace demo { struct Device { API int read() { return 7; } }; }\n";
    write(project.path(), "device.hpp", declaration);
    write(
        project.path(),
        "main.cpp",
        "#include \"device.hpp\"\nint entry(demo::Device* device) { return device->read(); }\n",
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["demo::Device::read"]);
    for replacement in [
        "#if !defined(__GNUC__)\n#define __attribute__(args)\n#endif\n",
        "#define __attribute__(args) __declspec(dllexport)\n",
    ] {
        write(project.path(), "compat.hpp", replacement);
        run();
        assert_eq!(targets(&store), ["demo::Device::read"], "{replacement}");
        assert_eq!(run().indexed, 0);
    }
    write(
        project.path(),
        "compat.hpp",
        "#define __attribute__(args) virtual\n",
    );
    run();
    assert!(
        targets(&store).is_empty(),
        "a semantic replacement must retract the normalized target"
    );
    std::fs::remove_file(project.path().join("compat.hpp")).unwrap();
    IncrementalPipeline::new(
        store.clone(),
        project.path().into(),
        ExtractionMode::Structural,
    )
    .sync(&NoopSink, &mut || false)
    .unwrap();
    assert_eq!(targets(&store), ["demo::Device::read"]);
    assert_eq!(
        std::fs::read_to_string(project.path().join("device.hpp")).unwrap(),
        declaration
    );
}

#[test]
fn ordinary_parenthesized_field_prefix_does_not_become_a_macro_limit() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "main.cpp",
        "using Number = int; struct Device { Number (saved) = 1; int read() { return saved; } }; int entry(Device* device) { return device->read(); }",
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        project.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    assert_eq!(targets(&store), ["Device::read"]);
}

#[test]
fn overlapping_annotation_roles_converge_and_survive_reindexing() {
    let project = tempfile::tempdir().unwrap();
    let source = "#define ANNOTATION(args)\nextern float item(float value, long double other)\n    UNKNOWN ANNOTATION ((unused));\nint leaf() { return 1; }\n";
    write(project.path(), "main.cpp", source);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    assert_eq!(run().indexed, 1);
    let check = || {
        let all = store.all_cpp_types().unwrap();
        let facts = &all[&types::FileId::generate("main.cpp")];
        let positions: Vec<_> = facts
            .normalized_annotations
            .iter()
            .map(|annotation| {
                assert_eq!(annotation.text, "ANNOTATION ((unused))");
                &annotation.position
            })
            .collect();
        assert_eq!(positions.len(), 2);
        assert!(positions.contains(&&types::cpp::CppAnnotationPosition::CallableSuffix));
        assert!(positions.contains(&&types::cpp::CppAnnotationPosition::DataSuffix));
        assert_eq!(store.find_symbols_by_qname("leaf").unwrap().len(), 1);
    };
    check();
    assert_eq!(run().indexed, 0);
    write(
        project.path(),
        "main.cpp",
        &format!("{source}int another() {{ return 2; }}\n"),
    );
    assert_eq!(run().indexed, 1);
    check();
    assert_eq!(store.find_symbols_by_qname("another").unwrap().len(), 1);
}

#[test]
fn complete_member_macros_are_independent_of_neighbor_declarations_and_revalidate() {
    let project = tempfile::tempdir().unwrap();
    let definition = "#define MEMBERS(C) C(); virtual ~C(); static int helper();\n";
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    for neighbor in [
        "enum { first = 1, second = 2 };",
        "enum class State { first, second };",
        "struct Nested { int value; };",
        "int saved = 0;",
        "int neighbor() { return 3; }",
    ] {
        for invocation in ["MEMBERS(Base)", "MEMBERS /* gap */ (Base) /* end */"] {
            let source = format!(
                "#include \"members.hpp\"\nnamespace demo {{ struct Base {{ {invocation} {neighbor} }}; struct Device : Base {{ int read() {{ return 7; }} }}; }}\nint entry(demo::Device* device) {{ return device->read(); }}\n"
            );
            write(project.path(), "main.cpp", &source);
            write(project.path(), "members.hpp", definition);
            run();
            assert_eq!(targets(&store), ["demo::Device::read"], "{source}");
            let types = store.all_cpp_types().unwrap();
            let facts = &types[&types::FileId::generate("main.cpp")];
            assert_eq!(facts.normalized_member_macros.len(), 1, "{source}");
            let site = &facts.normalized_member_macros[0];
            assert_eq!(site.scope, "demo::Base");
            assert_eq!(
                &source[site.range.start_byte as usize..site.range.end_byte as usize],
                site.text
            );
            let mut names: Vec<_> = facts
                .lookup_limits
                .iter()
                .filter(|limit| limit.declaration_range == site.range)
                .map(|limit| limit.name.as_deref())
                .collect();
            names.sort();
            assert_eq!(names, [Some("Base"), Some("helper"), Some("~Base")]);
            let frontend = extraction::cpp_annotations::frontend(
                facts.normalized_annotations.clone(),
                facts.normalized_member_macros.clone(),
            )
            .unwrap();
            let parsed = frontend.parser.parser_source(&source);
            assert_eq!(parsed.len(), source.len());
            assert_eq!(
                parsed
                    .match_indices('\n')
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>(),
                source
                    .match_indices('\n')
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>()
            );
            assert!(parsed.contains(neighbor), "{parsed}");
            assert_eq!(run().indexed, 0);

            // A complete macro can introduce the very name being investigated.
            // Its omission from parser input must never prove that name absent.
            write(
                project.path(),
                "members.hpp",
                &definition.replace("static int helper();", "virtual int read();"),
            );
            run();
            assert!(targets(&store).is_empty(), "{source}");

            // A fragment requiring the neighboring declaration to complete it,
            // a missing definition, and an unrelated same-name source cannot
            // justify a recovered connection.
            for unsupported in ["#define MEMBERS(C) int\n", ""] {
                write(project.path(), "members.hpp", unsupported);
                run();
                assert!(targets(&store).is_empty(), "{source}: {unsupported}");
                assert!(
                    store
                        .all_cpp_types()
                        .unwrap()
                        .values()
                        .all(|facts| facts.normalized_member_macros.is_empty())
                );
            }
            write(project.path(), "members.hpp", definition);
            run();
            assert_eq!(targets(&store), ["demo::Device::read"]);
        }
    }
}

#[test]
fn qualified_template_arguments_in_macros_preserve_calls_and_virtual_name_limits() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "main.cpp",
        "#include \"members.hpp\"\nstruct Base { MEMBERS(Base) };\nstruct Device : Base { int read() { return 7; } };\nint entry(Device* device) { return device->read(); }\n",
    );
    let definition = "namespace support { struct Token {}; template<class T> struct Box {}; }\n#define MEMBERS(C) C(); virtual ~C(); static support::Box<::support::Token> create();\n";
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    for separator in ["", " "] {
        write(
            project.path(),
            "members.hpp",
            &definition.replace("<::", &format!("<{separator}::")),
        );
        run();
        assert_eq!(targets(&store), ["Device::read"]);
    }
    // A comment inside the raw directive currently prevents retaining its
    // definition. Do not infer names from a partially recovered directive.
    write(
        project.path(),
        "members.hpp",
        &definition.replace("<::", "</* gap */::"),
    );
    run();
    assert!(targets(&store).is_empty());
    write(
        project.path(),
        "members.hpp",
        &definition.replace("create();", "create(); virtual int read();"),
    );
    run();
    assert!(targets(&store).is_empty());
    write(project.path(), "members.hpp", definition);
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    assert_eq!(run().indexed, 0);
}

#[test]
fn composed_member_invocations_preserve_names_and_retract_incomplete_declarations() {
    let project = tempfile::tempdir().unwrap();
    let source = "#include \"members.hpp\"\nstruct Base { MEMBERS(value) };\nstruct Device : Base { int read() { return 7; } };\nint entry(Device* device) { return device->read(); }\n";
    let definition = "#define FIELD(X) private: int X ## _ {0};\n#define GET(X) public: int get ## X() const { return X ## _; }\n#define MEMBERS(X) FIELD(X) GET(X)\n";
    write(project.path(), "main.cpp", source);
    write(project.path(), "members.hpp", definition);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    let facts = store.all_cpp_types().unwrap();
    let mut names: Vec<_> = facts
        .values()
        .flat_map(|f| &f.lookup_limits)
        .filter(|l| l.scope == "Base")
        .map(|l| l.name.as_deref())
        .collect();
    names.sort();
    assert_eq!(names, [Some("getvalue"), Some("value_")]);
    assert_eq!(run().indexed, 0);
    // A generated virtual remains relevant even without virtual/override on
    // the written derived method; composition must not erase that name.
    write(
        project.path(),
        "members.hpp",
        &definition.replace(
            "int get ## X() const { return X ## _; }",
            "virtual int read();",
        ),
    );
    run();
    assert!(targets(&store).is_empty());
    write(
        project.path(),
        "members.hpp",
        &definition.replace("int X ## _ {0};", "int X ## _ {0}"),
    );
    run();
    assert!(targets(&store).is_empty());
    write(project.path(), "members.hpp", definition);
    run();
    assert_eq!(targets(&store), ["Device::read"]);
}

#[test]
fn pasted_member_names_and_self_terminated_calls_preserve_virtual_limits() {
    let project = tempfile::tempdir().unwrap();
    let source = "#include \"members.hpp\"\nclass Base { public: MEMBERS(read) };\nclass Device : public Base { public: int read() { return 7; } };\nint entry(Device* device) { return device->read(); }\n";
    let definition = "#define MEMBERS(NAME) int saved ## NAME;\n";
    write(project.path(), "main.cpp", source);
    write(project.path(), "members.hpp", definition);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    let facts = store.all_cpp_types().unwrap();
    let site = facts
        .values()
        .flat_map(|f| &f.member_macros)
        .find(|s| s.name == "MEMBERS")
        .unwrap();
    assert_eq!(site.text, "MEMBERS(read)");
    assert_eq!(
        &source[site.range.start_byte as usize..site.range.end_byte as usize],
        site.text
    );
    assert!(
        facts
            .values()
            .flat_map(|f| &f.lookup_limits)
            .any(|l| l.scope == "Base" && l.name.as_deref() == Some("savedread"))
    );
    assert_eq!(run().indexed, 0);
    // A generated same-name virtual must prevent direct body selection even
    // when the written override does not repeat the virtual keyword.
    write(
        project.path(),
        "members.hpp",
        "#define MEMBERS(NAME) virtual int NAME();\n",
    );
    run();
    assert!(targets(&store).is_empty());
    write(
        project.path(),
        "members.hpp",
        "#define MEMBERS(NAME) int saved ## NAME\n",
    );
    run();
    assert!(
        targets(&store).is_empty(),
        "no caller semicolon can complete this replacement"
    );
    write(project.path(), "members.hpp", definition);
    run();
    assert_eq!(targets(&store), ["Device::read"]);
}

#[test]
fn attribute_class_heads_and_declaration_prefixes_preserve_nested_lookup() {
    let project = tempfile::tempdir().unwrap();
    let annotations =
        "#define API __attribute__((visibility(\"default\")))\n#define METHOD virtual\n";
    let source = r#"
#include "annotations.hpp"
namespace sample {
template<class T> struct Handle {};
class [[clang::lto_visibility_public]] API Boundary {
public:
    class Nested { public: int read() { return 7; } };
    API virtual int unrelated() { return 1; }
};
class Device : public Boundary::Nested {
public:
    API Device() {}
    [[nodiscard]] API int read() { return 11; }
    API Handle<Device> get();
    API static int value;
};
class Dynamic { public: METHOD int read() { return 9; } };
int dynamic(Dynamic* value) { return value->read(); }
}
int entry(sample::Device* device) { return device->read(); }
"#;
    write(project.path(), "main.cpp", source);
    write(project.path(), "annotations.hpp", annotations);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["sample::Device::read"]);
    let nested = store
        .find_symbols_by_qname("sample::Boundary::Nested")
        .unwrap();
    assert_eq!(nested.len(), 1);
    assert!(nested[0].range != nested[0].name_range);
    let virtuals = store
        .find_symbols_by_qname("sample::Boundary::unrelated")
        .unwrap();
    assert_eq!(virtuals.len(), 1);
    assert!(
        store
            .all_cpp_types()
            .unwrap()
            .values()
            .flat_map(|f| &f.callables)
            .any(|c| c.symbol_id == virtuals[0].id && c.is_virtual)
    );
    let facts = store.all_cpp_types().unwrap();
    let normalized: Vec<_> = facts
        .values()
        .flat_map(|f| &f.normalized_annotations)
        .collect();
    assert_eq!(normalized.len(), 6);
    assert!(normalized.iter().all(|site| site.text == "API"));
    for site in normalized {
        assert_eq!(
            &source[site.range.start_byte as usize..site.range.end_byte as usize],
            "API"
        );
    }
    let dynamic = store.find_symbols_by_qname("sample::dynamic").unwrap();
    assert_eq!(dynamic.len(), 1);
    assert!(
        store
            .find_edges_by_source(&dynamic[0].id)
            .unwrap()
            .iter()
            .all(|e| e.kind != types::EdgeKind::Calls)
    );
    assert_eq!(run().indexed, 0);
    write(
        project.path(),
        "annotations.hpp",
        "#define API UNVERIFIED\n#define METHOD virtual\n",
    );
    run();
    assert!(targets(&store).is_empty());
    assert!(
        store
            .all_cpp_types()
            .unwrap()
            .values()
            .all(|f| f.normalized_annotations.is_empty())
    );
    write(project.path(), "annotations.hpp", annotations);
    run();
    assert_eq!(targets(&store), ["sample::Device::read"]);
    assert_eq!(
        std::fs::read_to_string(project.path().join("main.cpp")).unwrap(),
        source
    );
}

#[test]
fn selected_annotation_wrappers_keep_namespace_members_and_runtime_expression_calls() {
    let project = tempfile::tempdir().unwrap();
    let annotations = r#"
#define ATTR(x) __attribute__((x))
#if 0
#define ONE(lock) UNKNOWN_ATTRIBUTE(lock)
#else
#define ONE(lock) ATTR(acquire_capability(lock)) ATTR(release_capability(lock))
#endif
#define TWO(lock, expr) (runtimeGuard(lock), expr)
#define SELECT(a, b, selected, ...) selected
#define MARK(...) SELECT(__VA_ARGS__, TWO, ONE, )(__VA_ARGS__)
"#;
    let source = r#"
#include "annotations.hpp"
namespace example {
struct __attribute__((capability("mutex"))) Lock {};
Lock gate;
void runtimeGuard(Lock&);
class Device { public: int read() { return 7; } };
class Owner { Device* device; public: int run(); };
auto callback() MARK(gate) {
    return [](bool value) MARK(gate) -> int { return value ? 1 : 0; };
}
int Owner::run() { return device->read(); }
int withRuntimeGuard() { return MARK(gate, callback()(true)); }
}
int entry(example::Device* device) { return device->read(); }
"#;
    write(project.path(), "main.cpp", source);
    write(project.path(), "annotations.hpp", annotations);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["example::Device::read"]);
    let callback = store.find_symbols_by_qname("example::callback").unwrap();
    assert_eq!(callback.len(), 1);
    assert!(callback[0].range != callback[0].name_range);
    assert!(
        store
            .find_symbols_by_qname("example::MARK")
            .unwrap()
            .is_empty()
    );
    let owner: Vec<_> = store
        .find_symbols_by_qname("example::Owner::run")
        .unwrap()
        .into_iter()
        .filter(|symbol| symbol.range != symbol.name_range)
        .collect();
    assert_eq!(owner.len(), 1);
    let edges = store.find_edges_by_source(&owner[0].id).unwrap();
    assert!(edges.iter().any(|edge| {
        store
            .find_symbol_by_id(&edge.target)
            .unwrap()
            .unwrap()
            .qualified_name
            == "example::Device::read"
    }));
    let facts = store.all_cpp_types().unwrap();
    let normalized: Vec<_> = facts
        .values()
        .flat_map(|facts| &facts.normalized_annotations)
        .collect();
    assert_eq!(normalized.len(), 2);
    assert!(
        normalized
            .iter()
            .all(|annotation| annotation.text == "MARK(gate)")
    );
    assert!(
        store
            .get_all_call_references()
            .unwrap()
            .iter()
            .any(|reference| reference.name == "MARK")
    );
    assert_eq!(run().indexed, 0);
    // Changing the selected alternative to an expression must retract the
    // parser-only normalization, without touching the source or a prior graph.
    write(
        project.path(),
        "annotations.hpp",
        &annotations.replace(
            "#define ONE(lock) ATTR(acquire_capability(lock)) ATTR(release_capability(lock))",
            "#define ONE(lock) runtimeGuard(lock)",
        ),
    );
    run();
    assert!(
        store
            .all_cpp_types()
            .unwrap()
            .values()
            .all(|facts| facts.normalized_annotations.is_empty())
    );
    write(project.path(), "annotations.hpp", annotations);
    run();
    assert_eq!(targets(&store), ["example::Device::read"]);
}

#[test]
fn recovered_typed_data_annotation_positions_preserve_member_lookup() {
    let project = tempfile::tempdir().unwrap();
    let source = r#"
#include "annotations.hpp"
struct __attribute__((capability("mutex"))) Lock {};
namespace storage {
template<class T> struct List {};
template<class K, class V> struct Map {};
}
class Device {
    Lock gate;
    auto makeCallback() const {
        return [this](int input) NEEDS(gate) -> int { return input; };
    }
    storage::List<int> waiting HELD(gate);
    storage::Map<int, storage::List<int>> values HELD(gate);
public:
    int read() const { return 7; }
};
int entry(Device* device) { return device->read(); }
Lock globalGate;
storage::List<int> pendingNotifications HELD(globalGate);
"#;
    let supported = "#define HELD(x) __attribute__((guarded_by(x)))\n#define NEEDS(x) __attribute__((requires_capability(x)))\n";
    write(project.path(), "main.cpp", source);
    write(project.path(), "annotations.hpp", supported);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    for member in ["Device::waiting", "Device::values"] {
        let symbols = store.find_symbols_by_qname(member).unwrap();
        assert_eq!(symbols.len(), 1, "{member}");
        assert_eq!(symbols[0].kind, types::SymbolKind::Field);
    }
    let global = store.find_symbols_by_qname("pendingNotifications").unwrap();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].kind, types::SymbolKind::Variable);
    let facts = store.all_cpp_types().unwrap();
    let normalized: Vec<_> = facts
        .values()
        .flat_map(|facts| &facts.normalized_annotations)
        .collect();
    assert_eq!(normalized.len(), 4);
    assert_eq!(
        normalized
            .iter()
            .filter(|site| site.position == types::cpp::CppAnnotationPosition::DataSuffix)
            .count(),
        3
    );
    assert_eq!(run().indexed, 0);
    for unsupported in [
        "#define HELD(x) __attribute__((requires_capability(x)))\n",
        "#define HELD(x) __attribute__((alias(\"another\")))\n",
        "#define HELD(x) UNKNOWN(x)\n",
    ] {
        write(
            project.path(),
            "annotations.hpp",
            &format!("#define NEEDS(x) __attribute__((requires_capability(x)))\n{unsupported}"),
        );
        run();
        assert!(targets(&store).is_empty(), "{unsupported}");
        write(project.path(), "annotations.hpp", supported);
        run();
        assert_eq!(targets(&store), ["Device::read"]);
    }
    for replacement in [
        "__attribute__((target(\"avx\")))",
        "__attribute__((guarded_by(x)))",
    ] {
        write(
            project.path(),
            "annotations.hpp",
            &format!(
                "#define HELD(x) __attribute__((guarded_by(x)))\n#define NEEDS(x) {replacement}\n"
            ),
        );
        run();
        // An unsupported annotation inside an independent callable body does
        // not erase the surrounding class's ordinary member declaration.
        assert_eq!(targets(&store), ["Device::read"], "{replacement}");
        assert!(
            store
                .all_cpp_types()
                .unwrap()
                .values()
                .flat_map(|facts| &facts.normalized_annotations)
                .all(|site| site.position != types::cpp::CppAnnotationPosition::LambdaSuffix),
            "{replacement} must remain unnormalized"
        );
        write(project.path(), "annotations.hpp", supported);
        run();
        assert_eq!(targets(&store), ["Device::read"]);
    }
}

#[test]
fn thread_annotation_wrappers_recover_members_with_actual_definitions() {
    let project = tempfile::tempdir().unwrap();
    let source = r#"
#include "annotations.hpp"
struct __attribute__((capability("mutex"))) Lock {};
class Device {
    Lock gate;
    Lock other;
    int value HELD(gate);
public:
    int read() const NEEDS(gate,
                          other) AVOIDS(other) { return value; }
};
int entry(Device* device) { return device->read(); }
"#;
    let supported = r#"
#define ATTR(x) __attribute__((x))
#define HELD(x) ATTR(guarded_by(x))
#define AVOIDS(...) ATTR(locks_excluded(__VA_ARGS__))
#if ANALYSIS_ENABLED
#define NEEDS(...) ATTR(requires_capability(__VA_ARGS__))
#else
#define NEEDS(...)
#endif
"#;
    write(project.path(), "main.cpp", source);
    write(project.path(), "unrelated.hpp", supported);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert!(targets(&store).is_empty());
    write(project.path(), "annotations.hpp", supported);
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    let declarations = store.all_cpp_types().unwrap();
    let normalized: Vec<_> = declarations
        .values()
        .flat_map(|facts| &facts.normalized_annotations)
        .cloned()
        .collect();
    for name in ["HELD", "NEEDS", "AVOIDS"] {
        assert!(
            normalized.iter().any(|site| site.name == name),
            "{normalized:?}"
        );
    }
    let frontend = extraction::cpp_annotations::frontend(normalized, Vec::new()).unwrap();
    let parsed = frontend.parser.parser_source(source);
    assert_eq!(parsed.len(), source.len());
    assert_eq!(
        parsed
            .match_indices('\n')
            .map(|(i, _)| i)
            .collect::<Vec<_>>(),
        source
            .match_indices('\n')
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    );
    let members = store.find_symbols_by_qname("Device::value").unwrap();
    assert_eq!(members.len(), 1);
    let read = store.find_symbols_by_qname("Device::read").unwrap();
    assert_eq!(read.len(), 1);
    assert!(
        source[read[0].range.start_byte as usize..read[0].range.end_byte as usize]
            .ends_with("{ return value; }")
    );
    assert_eq!(run().indexed, 0);
    for bad in [
        "#undef NEEDS\n",
        "#define NEEDS(...) __attribute__((alias(\"another\")))\n",
        "#define NEEDS(...) int extra;\n",
        "#define ATTR(x) __attribute__((x, alias(\"another\")))\n",
        "#define gate injected, tokens\n",
        "#define HELD(x) ATTR(requires_capability(x))\n",
    ] {
        write(
            project.path(),
            "annotations.hpp",
            &format!("{supported}\n{bad}"),
        );
        run();
        assert!(targets(&store).is_empty(), "{bad}");
        write(project.path(), "annotations.hpp", supported);
        run();
        assert_eq!(targets(&store), ["Device::read"]);
    }
}

#[test]
fn callable_suffix_annotations_recover_members_and_retract_unsupported_positions() {
    let project = tempfile::tempdir().unwrap();
    let source = r#"
#include "export.hpp"
class Device {
public:
    Device() EXPORT;
    static int tag() EXPORT { return 7; }
    int read() const EXPORT;
};
int Device::read() const { return tag(); }
int entry(Device* device) { return device->read(); }
"#;
    write(project.path(), "main.cpp", source);
    // An unrelated same-name definition cannot supply the missing include.
    let supported =
        "#define EXPORTED __attribute__((visibility(\"default\")))\n#define EXPORT EXPORTED\n";
    write(project.path(), "unrelated.hpp", supported);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert!(targets(&store).is_empty());
    write(project.path(), "export.hpp", supported);
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    let declarations = store.all_cpp_types().unwrap();
    let normalized: Vec<_> = declarations
        .values()
        .flat_map(|facts| &facts.normalized_annotations)
        .collect();
    assert_eq!(normalized.len(), 3);
    for token in normalized {
        assert_eq!(
            token.position,
            types::cpp::CppAnnotationPosition::CallableSuffix
        );
        assert_eq!(
            &source[token.range.start_byte as usize..token.range.end_byte as usize],
            "EXPORT"
        );
    }
    let tag = store.find_symbols_by_qname("Device::tag").unwrap();
    assert_eq!(tag.len(), 1);
    assert!(tag[0].static_);
    let body = &source[tag[0].range.start_byte as usize..tag[0].range.end_byte as usize];
    assert!(body.contains("EXPORT { return 7; }"));
    assert_eq!(run().indexed, 0);
    for unsupported in [
        "#define EXPORT __declspec(dllexport)\n",
        "#define EXPORT __attribute__((alias(\"other\")))\n",
        "#define EXPORT(X)\n",
        "#define EXPORT EXPORTED\n#undef EXPORTED\n",
    ] {
        write(project.path(), "export.hpp", unsupported);
        run();
        assert!(targets(&store).is_empty(), "{unsupported}");
        write(project.path(), "export.hpp", supported);
        run();
        assert_eq!(targets(&store), ["Device::read"]);
    }
}

#[test]
fn member_access_labels_preserve_inherited_lookup_and_private_virtual_hiding() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "main.cpp",
        r#"
#include "members.hpp"
struct Base { int read() { return 7; } };
struct Device : Base {
    MEMBERS(Device);
public:
    int entry() { return read(); }
};
"#,
    );
    let supported = "#define MEMBERS(C) public: static C& instance(); C(const C&) = delete; C& operator=(const C&) = delete; private: int state_;\n";
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    let outgoing = || {
        let entry = store.find_symbols_by_qname("Device::entry").unwrap();
        assert_eq!(entry.len(), 1);
        store
            .find_edges_by_source(&entry[0].id)
            .unwrap()
            .into_iter()
            .filter(|edge| edge.kind == types::EdgeKind::Calls)
            .map(|edge| {
                store
                    .find_symbol_by_id(&edge.target)
                    .unwrap()
                    .unwrap()
                    .qualified_name
            })
            .collect::<Vec<_>>()
    };
    run();
    assert!(outgoing().is_empty());
    write(project.path(), "members.hpp", supported);
    run();
    assert_eq!(outgoing(), ["Base::read"]);
    assert_eq!(run().indexed, 0);
    for replacement in [
        "#define MEMBERS(C) private: virtual int read() { return 9; }\n",
        "#define MEMBERS(C) protected: virtual int read() { return 9; }\n",
        "#define MEMBERS(C) public: UNKNOWN(C);\n",
        "#define MEMBERS(C) public: int state_;\n#undef MEMBERS\n",
    ] {
        write(project.path(), "members.hpp", replacement);
        run();
        assert!(outgoing().is_empty(), "{replacement}");
        write(project.path(), "members.hpp", supported);
        run();
        assert_eq!(outgoing(), ["Base::read"]);
    }
}

#[test]
fn composed_lifecycle_declarations_restore_calls_and_keep_nested_hiding_unknown() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "main.cpp",
        r#"
#include "lifecycle.hpp"
struct Device {
public:
    LIFECYCLE(Device);
    int read() { return 7; }
};
int entry(Device* device) { return device->read(); }
"#,
    );
    let supported = "#define COPY(C) C(const C&) = delete; C& operator=(const C&) = delete\n#define MOVE(C) C(C&&) = delete; C& operator=(C&&) = delete\n#define LIFECYCLE(C) COPY(C); MOVE(C)\n";
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert!(targets(&store).is_empty());
    write(project.path(), "lifecycle.hpp", supported);
    run();
    assert_eq!(targets(&store), ["Device::read"]);
    assert_eq!(run().indexed, 0);
    for macros in [
        format!("{supported}\n#undef MOVE\n"),
        "#define MOVE(C) virtual int read(int value) { return value; }\n#define LIFECYCLE(C) MOVE(C)\n".into(),
        "#define MOVE(C) LIFECYCLE(C)\n#define LIFECYCLE(C) MOVE(C)\n".into(),
    ] {
        write(project.path(), "lifecycle.hpp", &macros);
        run();
        assert!(targets(&store).is_empty(), "{macros}");
        write(project.path(), "lifecycle.hpp", supported);
        run();
        assert_eq!(targets(&store), ["Device::read"]);
    }
}

#[test]
fn member_macro_inventory_recovers_inherited_type_lookup_and_retracts_on_change() {
    let project = tempfile::tempdir().unwrap();
    let supported = "#define MEMBERS(TEXT) static constexpr const char16_t *label_ = TEXT; static const char16_t* label() { return label_; }\n";
    write(project.path(), "members.hpp", supported);
    let source = r#"
#include "members.hpp"
namespace demo {
template<class T> struct Handle { T* operator->() const { return pointer; } T* pointer; };
struct Payload { int read() { return 7; } };
struct Contract { public: MEMBERS(u"descriptor,with-comma"); };
template<class T> struct Layer : T {};
struct Owner : Layer<Contract> {
    Handle<Payload> payload;
    int entry() { return payload->read(); }
};
}
"#;
    write(project.path(), "main.cpp", source);
    write(
        project.path(),
        "unrelated.hpp",
        "#define MEMBERS(X) Unknown\n",
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    let outgoing = || {
        let owner = store.find_symbols_by_qname("demo::Owner::entry").unwrap();
        assert_eq!(owner.len(), 1);
        store
            .find_edges_by_source(&owner[0].id)
            .unwrap()
            .into_iter()
            .filter(|edge| edge.kind == types::EdgeKind::Calls)
            .map(|edge| {
                store
                    .find_symbol_by_id(&edge.target)
                    .unwrap()
                    .unwrap()
                    .qualified_name
            })
            .collect::<Vec<_>>()
    };
    run();
    let initial = outgoing();
    assert!(
        initial.contains(&"demo::Payload::read".into()),
        "{initial:?}"
    );
    assert!(initial.contains(&"demo::Handle::operator->".into()));
    assert_eq!(run().indexed, 0);
    for replacement in [
        "#define MEMBERS(TEXT) Unknown(TEXT)\n",
        "#define MEMBERS(TEXT) static int Handle;\n",
        "#define MEMBERS(TEXT) static int label_;\n#undef MEMBERS\n",
    ] {
        write(project.path(), "members.hpp", replacement);
        run();
        assert!(outgoing().is_empty(), "{replacement}: {:?}", outgoing());
        write(project.path(), "members.hpp", supported);
        run();
        assert_eq!(outgoing(), initial);
    }
    let file = store
        .list_files()
        .unwrap()
        .into_iter()
        .find(|file| file.path == "main.cpp")
        .unwrap();
    let facts = store.cpp_types_for_file(&file.file_id).unwrap().unwrap();
    let site = &facts.member_macros[0];
    let names: Vec<_> = facts
        .lookup_limits
        .iter()
        .filter(|limit| limit.declaration_range == site.range)
        .map(|limit| limit.name.as_deref())
        .collect();
    assert_eq!(names, [Some("label"), Some("label_")]);
    assert_eq!(
        &source[site.range.start_byte as usize..site.range.end_byte as usize],
        site.text
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("main.cpp")).unwrap(),
        source
    );
}

#[test]
fn generated_member_names_block_written_overload_selection() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "main.cpp",
        r#"
#define MEMBERS(TEXT) int read(int value) { return value; }
struct Device { MEMBERS(u"descriptor"); int read() { return 1; } };
int entry(Device* device) { return device->read(); }
"#,
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        project.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    assert!(
        targets(&store).is_empty(),
        "generated declarations are retained as lookup limits, not silently omitted from overload selection"
    );
}

#[test]
fn actual_header_annotations_recover_class_calls_and_macro_changes_invalidate_them() {
    let project = tempfile::tempdir().unwrap();
    let macros = "#if PLATFORM_A\n#define DETAIL __attribute__((visibility(\"default\")))\n#else\n#define DETAIL __declspec(dllexport)\n#endif\n#define API DETAIL\n";
    write(project.path(), "attributes.hpp", macros);
    let declaration = "#include \"attributes.hpp\"\nnamespace demo { class API Device final { public: int read() const { return 7; } }; }";
    write(project.path(), "device.hpp", declaration);
    write(project.path(), "unrelated.hpp", "#define API WrongName\n");
    write(
        project.path(),
        "main.cpp",
        "#include \"missing.hpp\"\n#include \"device.hpp\"\nint entry(demo::Device* device) { return device->read(); }",
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run();
    assert_eq!(targets(&store), ["demo::Device::read"]);
    let header = store
        .list_files()
        .unwrap()
        .into_iter()
        .find(|f| f.path == "device.hpp")
        .unwrap();
    let facts = store.cpp_types_for_file(&header.file_id).unwrap().unwrap();
    assert_eq!(facts.normalized_annotations.len(), 1);
    let at = facts.normalized_annotations[0].range;
    assert_eq!(
        &declaration[at.start_byte as usize..at.end_byte as usize],
        "API"
    );
    assert_eq!(
        run().indexed,
        0,
        "unchanged inputs must reuse the prepared facts"
    );
    assert_eq!(targets(&store), ["demo::Device::read"]);

    write(project.path(), "attributes.hpp", "#define API UNKNOWN\n");
    run();
    assert!(
        targets(&store).is_empty(),
        "the unchanged caller cannot retain its old normalized target"
    );
    assert!(
        store
            .cpp_types_for_file(&header.file_id)
            .unwrap()
            .unwrap()
            .normalized_annotations
            .is_empty()
    );
    write(project.path(), "attributes.hpp", macros);
    IncrementalPipeline::new(
        store.clone(),
        project.path().into(),
        ExtractionMode::Structural,
    )
    .sync(&NoopSink, &mut || false)
    .unwrap();
    assert_eq!(targets(&store), ["demo::Device::read"]);
    assert_eq!(
        std::fs::read_to_string(project.path().join("device.hpp")).unwrap(),
        declaration
    );
}

#[test]
fn annotation_visibility_respects_include_order_and_earlier_local_definitions() {
    let project = tempfile::tempdir().unwrap();
    write(
        project.path(),
        "good/attrs.hpp",
        "#define API __attribute__((visibility(\"default\")))\n",
    );
    write(project.path(), "bad/attrs.hpp", "#define API UNKNOWN\n");
    write(
        project.path(),
        "device.hpp",
        "#include <attrs.hpp>\nnamespace demo { class API Device final { public: int read() { return 0; } }; }",
    );
    write(
        project.path(),
        "main.cpp",
        "#include \"device.hpp\"\nint entry(demo::Device* device) { return device->read(); }",
    );
    write(
        project.path(),
        "later.hpp",
        "class API Later { int value; };\n#include <attrs.hpp>\n",
    );
    write(
        project.path(),
        "undefined.hpp",
        "#include <attrs.hpp>\n#undef API\nclass API Missing { int value; };\n",
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    for (directories, connected) in [
        (vec!["good", "bad"], true),
        (vec!["bad", "good"], false),
        (vec!["good"], true),
    ] {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural)
                .with_include_paths(directories.into_iter().map(Into::into).collect()),
        )
        .run(&NoopSink, &mut || false)
        .unwrap();
        assert_eq!(!targets(&store).is_empty(), connected);
        for path in ["later.hpp", "undefined.hpp"] {
            let file = store
                .list_files()
                .unwrap()
                .into_iter()
                .find(|f| f.path == path)
                .unwrap();
            assert!(
                store
                    .cpp_types_for_file(&file.file_id)
                    .unwrap()
                    .unwrap()
                    .normalized_annotations
                    .is_empty(),
                "{path}"
            );
        }
    }
}

#[test]
fn annotation_preparation_cancellation_retries_unchanged_callers_and_remaining_files() {
    let project = tempfile::tempdir().unwrap();
    write(project.path(), "attrs.hpp", "#define API UNKNOWN\n");
    let batch_size = extraction::extraction_pool().current_num_threads().max(1);
    for index in 0..=batch_size {
        let name = format!("Device{index}");
        write(
            project.path(),
            &format!("{name}.hpp"),
            &format!(
                "#include \"attrs.hpp\"\nclass API {name} final {{ public: int read() {{ return 7; }} }};"
            ),
        );
    }
    write(
        project.path(),
        "main.cpp",
        &format!(
            "#include \"Device0.hpp\"\n#include \"Device{batch_size}.hpp\"\nint entry(Device0* device, Device{batch_size}* peer) {{ return device->read() + peer->read(); }}"
        ),
    );
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let pipeline = || {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
    };
    pipeline().run(&NoopSink, &mut || false).unwrap();
    assert!(targets(&store).is_empty());
    write(
        project.path(),
        "attrs.hpp",
        "#define API __attribute__((visibility(\"default\")))\n",
    );
    let prepared = || {
        store
            .all_cpp_types()
            .unwrap()
            .values()
            .filter(|f| !f.normalized_annotations.is_empty())
            .count()
    };
    assert!(pipeline().run(&NoopSink, &mut || prepared() > 0).is_err());
    assert_eq!(
        prepared(),
        batch_size,
        "cancellation stops after one atomic batch"
    );
    pipeline().run(&NoopSink, &mut || false).unwrap();
    assert_eq!(prepared(), batch_size + 1);
    let mut connected = targets(&store);
    connected.sort();
    assert_eq!(
        connected,
        [
            "Device0::read".to_string(),
            format!("Device{batch_size}::read")
        ]
    );
}

#[test]
fn stringized_member_inventory_unblocks_types_and_revalidates_actual_headers() {
    let project = tempfile::tempdir().unwrap();
    let supported = "#define MEMBERS(X) public: virtual const char* label() { return #X; }\n";
    let source = r#"
#include "members.hpp"
template<class T> struct Handle { T* operator->() const { return pointer; } T* pointer; };
struct Payload { int read() { return 7; } };
struct Contract { MEMBERS(Contract); };
struct Owner : Contract {
    Handle<Payload> payload;
    int invoke() { return payload->read(); }
};
"#;
    write(project.path(), "main.cpp", source);
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    for (header, expected) in [
        ("", false),
        (supported, true),
        (
            "#define MEMBERS(X) const char* label() { return #X; } int Handle;\n",
            false,
        ),
        (supported, true),
        (
            "#define MEMBERS(X) const char* label() { return #X; }\n#undef MEMBERS\n",
            false,
        ),
        (supported, true),
    ] {
        write(project.path(), "members.hpp", header);
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural),
        )
        .run(&NoopSink, &mut || false)
        .unwrap();
        let calls = store.get_all_call_references().unwrap();
        let call = calls.iter().find(|call| call.name == "read").unwrap();
        assert_eq!(call.resolved.is_some(), expected, "{header}: {call:?}");
        if let Some(target) = &call.resolved {
            assert_eq!(
                store
                    .find_symbol_by_id(&target.symbol_id)
                    .unwrap()
                    .unwrap()
                    .qualified_name,
                "Payload::read"
            );
        }
        // Name inventory does not publish a macro's expanded body or target.
        assert!(
            store
                .find_symbols_by_qname("Contract::label")
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        std::fs::read_to_string(project.path().join("main.cpp")).unwrap(),
        source
    );
}
