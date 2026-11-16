#![cfg(feature = "cpp")]

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use std::{collections::HashSet, path::Path};
use types::{FileId, Language, ReferenceKind};

#[test]
fn nested_arguments_do_not_borrow_a_nearby_call_target() {
    use types::{DataFlowKind, DataNodeKind};
    for callee in ["Consume", "Output::Consume"] {
        let source = format!(
            r#"
int first() {{ return 1; }}
int second() {{ return 2; }}
void Consume(int, int);
namespace Output {{ void Consume(int, int); }}
void run() {{ {callee}(first(), second()); }}
"#
        );
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("arguments.cpp"),
            Path::new("arguments.cpp"),
            &source,
            "arguments",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let reference = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "Consume")
            .unwrap();
        let call = facts
            .callsites
            .iter()
            .find(|c| c.reference_id == Some(reference.id))
            .unwrap();
        assert_eq!(call.args.len(), 2);
        for (index, text) in ["first()", "second()"].into_iter().enumerate() {
            let argument = &call.args[index];
            assert_eq!(argument.value, text);
            let node = facts
                .data_nodes
                .iter()
                .find(|n| Some(n.id) == argument.data_node_id)
                .unwrap();
            assert_eq!(node.kind, DataNodeKind::CallArg);
            assert_eq!(node.callsite_id, Some(call.id));
            let edges: Vec<_> = facts
                .dataflow_edges
                .iter()
                .filter(|edge| edge.kind == DataFlowKind::ArgToCall && edge.source == node.id)
                .collect();
            if callee == "Consume" {
                assert!(!edges.is_empty(), "ordinary call remains connected");
            }
            for edge in edges {
                let target = facts
                    .data_nodes
                    .iter()
                    .find(|n| n.id == edge.target)
                    .unwrap();
                assert_eq!(
                    target.callsite_id,
                    Some(call.id),
                    "argument must not attach to first() or second()"
                );
                assert_eq!(target.name.as_deref(), Some("Consume"));
            }
        }
    }
}

#[test]
fn recovered_cross_statement_calls_become_located_gaps_without_losing_inner_calls() {
    for operator in ["=", "+=", "*="] {
        let source = format!(
            r#"
#define PROPERTY(Type, Op) Type value{{}}; value Op 1; return value;
struct Node {{}};
template<class T> T* Acquire(int);
namespace Other {{ template<class T> T* Acquire(int); }}
void use(Node*);
struct Record {{ int Read() const; }};
int Record::Read() const {{ PROPERTY(int, {operator}); }}
void Visit(bool ready) {{
    auto node = ready ? Acquire<Node>(1) : Other::Acquire<Node>(2);
    if (!node) {{ return; }}
    use(node);
}}
"#
        );
        for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
            let file = FileId::generate("recovery.cpp");
            let facts = extract_file_with_mode(
                &create_frontend(Language::Cpp).unwrap(),
                file,
                Path::new("recovery.cpp"),
                &source,
                "test",
                mode,
                &(),
            )
            .unwrap();
            let start = source.find("ready ? Acquire").unwrap() as u32;
            let end = (source.find("if (!node)").unwrap() + "if (!node)".len()) as u32;
            assert!(
                facts.diagnostics.iter().any(|d| {
                    d.message
                        .starts_with("Recovered callee crosses a statement boundary")
                        && d.range
                            .is_some_and(|r| r.start_byte == start && r.end_byte == end)
                }),
                "{operator}: {:#?}",
                facts.diagnostics
            );
            let calls: Vec<_> = facts
                .references
                .iter()
                .filter(|r| r.kind == ReferenceKind::Call)
                .collect();
            assert!(!calls.iter().any(|r| r.range.start_byte == start));
            assert!(calls.iter().any(|r| r.text == "Acquire<Node>"));
            assert!(calls.iter().any(|r| r.text == "Other::Acquire<Node>"));
            assert!(calls.iter().any(|r| r.name == "use"));
            assert!(
                !facts
                    .callsites
                    .iter()
                    .any(|c| c.range.start_byte == start && c.range.end_byte == end)
            );
            let rejected = types::CallsiteId::from_file_range(&file, start, end);
            assert!(
                !facts
                    .data_nodes
                    .iter()
                    .any(|n| n.callsite_id == Some(rejected))
            );
            assert!(
                !facts
                    .data_nodes
                    .iter()
                    .any(|n| n.kind == types::DataNodeKind::CallReturn
                        && n.range.start_byte == start
                        && n.range.end_byte == end),
                "recovery spanning statements must not become a result boundary"
            );
        }
    }
}

#[test]
fn nested_statements_literals_and_comments_do_not_remove_expression_calls() {
    let source = r##"
using Function = void (*)(int);
Function fetch(const char*);
void target(int);
void Visit() {
    ([](int) { target(1); })(2);
    ({ target(3); &target; })(4);
    fetch(";")(5);
    fetch(R"(;)")(6);
    fetch /* ; */ ("x")(7);
}
"##;
    for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("nested.cpp"),
            Path::new("nested.cpp"),
            source,
            "test",
            mode,
            &(),
        )
        .unwrap();
        assert!(!facts.diagnostics.iter().any(|d| {
            d.message
                .starts_with("Recovered callee crosses a statement boundary")
        }));
        for expression in [
            "([](int) { target(1); })(2)",
            "({ target(3); &target; })(4)",
            "fetch(\";\")(5)",
            "fetch(R\"(;)\")(6)",
            "fetch /* ; */ (\"x\")(7)",
            "target(1)",
            "target(3)",
        ] {
            let start = source.find(expression).unwrap() as u32;
            assert!(
                facts.callsites.iter().any(|c| c.range.start_byte == start
                    && c.range.end_byte == start + expression.len() as u32),
                "missing {expression}: {:#?}",
                facts.callsites
            );
        }
    }
}

#[test]
fn callee_boundaries_keep_written_tokens_without_changing_ordinary_calls() {
    let source = "// 中文\r\nstruct Value {\r\n bool operator==(const Value&) const;\r\n bool check(const Value& other) const { return this->operator==(other); }\r\n bool spaced(const Value& other) const { return this /* gap */ ->operator== (other); }\r\n};\r\nvoid plain(int);\r\nvoid invoke(Value* pointer, Value value) {\r\n pointer->operator==(value);\r\n value.operator==(value);\r\n plain /* comment */ (1);\r\n plain\\\n(2);\r\n}\r\n";
    for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("boundaries.cpp"),
            Path::new("boundaries.cpp"),
            source,
            "test",
            mode,
            &(),
        )
        .unwrap();
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call)
            .collect();
        assert_eq!(calls.len(), 6, "{calls:#?}");
        for expression in [
            "this->operator==",
            "this /* gap */ ->operator== ",
            "pointer->operator==",
            "value.operator==",
        ] {
            let call = calls
                .iter()
                .find(|r| r.text == expression)
                .unwrap_or_else(|| panic!("missing {expression}: {calls:#?}"));
            assert_eq!(
                &source[call.range.start_byte as usize..call.range.end_byte as usize],
                expression
            );
            assert_eq!(call.name, expression);
            assert_eq!(call.arity, Some(1));
            assert!(call.receiver.is_none());
            let site = facts
                .callsites
                .iter()
                .find(|s| s.reference_id == Some(call.id))
                .unwrap();
            assert_eq!(site.args.len(), 1);
            assert!(site.range.end_byte > call.range.end_byte);
        }
        assert_eq!(
            calls
                .iter()
                .filter(|r| r.name == "plain" && r.text == "plain")
                .count(),
            2
        );
        assert!(
            facts
                .diagnostics
                .iter()
                .any(|d| d.message.starts_with("Parser could not recognize"))
        );
    }
}

#[test]
fn expression_calls_preserve_inner_and_outer_sites_and_arguments() {
    let source = r#"
using Callback = void (*)(int);
Callback make(int);
struct Event { Callback callback(); };
void run(Event* event, Callback* callbacks, Callback chosen, bool flag) {
    make(11);
    make(12)(21);
    (event->callback())(22);
    callbacks[0](23);
    (*chosen)(24);
    (flag ? chosen : callbacks[1])(25);
    auto pending = [event] { (event->callback())(26); };
}
"#;
    for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("expressions.cpp"),
            Path::new("expressions.cpp"),
            source,
            "test",
            mode.clone(),
            &(),
        )
        .unwrap();
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call)
            .collect();
        assert_eq!(calls.len(), 10, "{calls:#?}");
        assert_eq!(facts.callsites.len(), 10);
        assert_eq!(
            facts
                .callsites
                .iter()
                .map(|c| c.id)
                .collect::<HashSet<_>>()
                .len(),
            10
        );
        let run = facts.symbols.iter().find(|s| s.name == "run").unwrap();
        for (expression, full, arguments) in [
            ("make", "make(11)", vec!["11"]),
            ("make(12)", "make(12)(21)", vec!["21"]),
            ("event->callback", "event->callback()", vec![]),
            ("(event->callback())", "(event->callback())(22)", vec!["22"]),
            ("callbacks[0]", "callbacks[0](23)", vec!["23"]),
            ("(*chosen)", "(*chosen)(24)", vec!["24"]),
            (
                "(flag ? chosen : callbacks[1])",
                "(flag ? chosen : callbacks[1])(25)",
                vec!["25"],
            ),
        ] {
            let call = calls.iter().find(|r| r.text == expression).unwrap();
            let site = facts
                .callsites
                .iter()
                .find(|c| c.reference_id == Some(call.id))
                .unwrap();
            assert_eq!(
                &source[site.range.start_byte as usize..site.range.end_byte as usize],
                full
            );
            assert_eq!(
                site.args
                    .iter()
                    .map(|a| a.value.as_str())
                    .collect::<Vec<_>>(),
                arguments
            );
            assert_eq!(call.arity, Some(arguments.len() as u32));
            assert_eq!(call.source_symbol, Some(run.id));
        }
        let inner = calls
            .iter()
            .find(|r| {
                r.text == "make" && r.range.start_byte as usize == source.find("make(12)").unwrap()
            })
            .unwrap();
        let site = facts
            .callsites
            .iter()
            .find(|c| c.reference_id == Some(inner.id))
            .unwrap();
        assert_eq!(site.args[0].value, "12");
        assert_eq!(
            &source[site.range.start_byte as usize..site.range.end_byte as usize],
            "make(12)"
        );
        if matches!(mode, ExtractionMode::Full) {
            let argument = facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == types::DataNodeKind::CallArg && node.name.as_deref() == Some("12")
                })
                .unwrap();
            assert_eq!(
                argument.callsite_id,
                Some(site.id),
                "inner argument must not be attached to outer returned-callable invocation"
            );
            let outer = facts
                .callsites
                .iter()
                .find(|c| {
                    &source[c.range.start_byte as usize..c.range.end_byte as usize]
                        == "make(12)(21)"
                })
                .unwrap();
            let argument = facts
                .data_nodes
                .iter()
                .find(|node| {
                    node.kind == types::DataNodeKind::CallArg && node.name.as_deref() == Some("21")
                })
                .unwrap();
            assert_eq!(
                argument.callsite_id,
                Some(outer.id),
                "outer argument must not be attached to inner getter invocation"
            );
        }
        let pending = facts.cpp_types.as_ref().unwrap().lambda_captures[0].symbol_id;
        let offset = source.find("(event->callback())(26)").unwrap() as u32;
        let nested: Vec<_> = calls
            .iter()
            .filter(|r| r.range.start_byte >= offset)
            .collect();
        assert_eq!(nested.len(), 2);
        assert!(nested.iter().all(|r| r.source_symbol == pending));
        assert!(
            !calls.iter().any(|r| r.name.starts_with("<lambda@")),
            "definition is not invocation"
        );
    }
}

#[test]
fn qualified_names_have_no_query_depth_limit_and_other_callees_stay_whole() {
    let source = "void run() { a::b::c::d::e::call(1); object.Base::method(); templated<int>(); (named)(2); }";
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("names.cpp"),
        Path::new("names.cpp"),
        source,
        "test",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Call)
        .collect();
    assert_eq!(calls.len(), 4, "{calls:#?}");
    let ordinary = calls.iter().find(|r| r.name == "call").unwrap();
    assert_eq!(ordinary.text, "a::b::c::d::e::call");
    assert_eq!(ordinary.receiver.as_deref(), Some("a::b::c::d::e"));
    for expression in ["object.Base::method", "templated<int>", "(named)"] {
        let call = calls.iter().find(|r| r.text == expression).unwrap();
        assert_eq!(call.name, expression);
        assert_eq!(call.receiver, None);
    }
}

#[test]
fn named_casts_are_not_explicit_calls_but_operand_and_result_calls_remain() {
    let source = r#"
void run() {
    static_cast<int>(get());
    reinterpret_cast<long>(ptr);
    const_cast<int*>(ptr);
    dynamic_cast<Derived*>(base);
    static_cast /* comment */ <long>(get());
    (*reinterpret_cast<Callback*>(ptr))(42);
    std::static_pointer_cast<Derived>(shared);
    auto value = int(get());
    double(3);
}
"#;
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("casts.cpp"),
        Path::new("casts.cpp"),
        source,
        "test",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Call)
        .collect();
    assert_eq!(
        calls.len(),
        5,
        "casts are conversion syntax, not explicit function calls: {calls:#?}"
    );
    assert_eq!(calls.iter().filter(|r| r.text == "get").count(), 3);
    assert!(
        calls
            .iter()
            .any(|r| r.text == "(*reinterpret_cast<Callback*>(ptr))" && r.arity == Some(1))
    );
    assert!(
        calls
            .iter()
            .any(|r| r.text == "std::static_pointer_cast<Derived>" && r.arity == Some(1))
    );
}

#[test]
fn attribute_names_are_not_calls_but_argument_expressions_and_same_named_calls_remain() {
    let source = r#"
constexpr int alignment() { return 16; }
int actual();
void aligned(int);
void __attribute__((visibility("default"), deprecated("note"))) api();
struct __attribute__((packed, aligned(alignment()))) Payload { int value; };
void run() {
    __attribute__((aligned((alignment())), unused)) int local = actual();
    alignas(alignment()) int other = actual();
    [[gnu::aligned(alignment())]] int standard = actual();
    __attribute__((aligned([] { return alignment(); }()))) int closure = actual();
    aligned(16);
}
"#;
    let frontend = create_frontend(Language::Cpp).unwrap();
    let file = FileId::generate("attributes.cpp");
    let extract = |mode| {
        extract_file_with_mode(
            &frontend,
            file,
            Path::new("attributes.cpp"),
            source,
            "attributes",
            mode,
            &(),
        )
        .unwrap()
    };
    let structural = extract(ExtractionMode::Structural);
    let full = extract(ExtractionMode::Full);
    let run = structural.symbols.iter().find(|s| s.name == "run").unwrap();
    let unit = types::lazy::AnalysisUnit::from_function(file, run.id, run.range);
    let lazy = extract(ExtractionMode::LazyDataflow {
        include_parameter_outputs: true,
        window: types::lazy::LazyWindow {
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
        callsites: structural.callsites.clone(),
    });
    let expected: HashSet<_> = source
        .match_indices("alignment()")
        .filter(|(at, _)| *at != source.find("alignment()").unwrap())
        .map(|(at, _)| at as u32)
        .chain(
            source
                .match_indices("actual()")
                .skip(1)
                .map(|(at, _)| at as u32),
        )
        .chain([
            source.find("[] {").unwrap() as u32,
            source.find("aligned(16)").unwrap() as u32,
        ])
        .collect();
    for facts in [&structural, &full] {
        let calls: HashSet<_> = facts
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call)
            .map(|r| r.range.start_byte)
            .collect();
        assert_eq!(
            calls, expected,
            "attribute metadata must not add calls: {:#?}",
            facts.references
        );
        assert_eq!(facts.callsites.len(), expected.len());
    }
    // Lazy extraction consumes existing structural callsites and deliberately
    // omits them from its result; its data nodes still reference those IDs.
    for (facts, callsites) in [(&full, &full.callsites), (&lazy, &structural.callsites)] {
        let call_ids: HashSet<_> = callsites.iter().map(|s| s.id).collect();
        for node in &facts.data_nodes {
            if node.kind == types::DataNodeKind::CallReturn {
                // Result boundaries have their own location identity and may
                // have no callsite_id. Only real expression calls may produce one.
                assert!(expected.contains(&node.range.start_byte), "{node:#?}");
            }
            if matches!(
                node.kind,
                types::DataNodeKind::CallTarget
                    | types::DataNodeKind::CallArg
                    | types::DataNodeKind::CallOutput
            ) {
                assert!(
                    node.callsite_id.is_some_and(|id| call_ids.contains(&id)),
                    "attribute parameters are not invocation arguments or returns: {node:#?}"
                );
            }
        }
        let first_local_call = source.find("alignment())), unused").unwrap() as u32;
        assert!(
            facts
                .data_nodes
                .iter()
                .any(|n| n.kind == types::DataNodeKind::CallReturn
                    && n.range.start_byte == first_local_call),
            "nested expression remains a result"
        );
    }
}

#[test]
fn explicit_gnu_attribute_lists_are_not_calls_when_the_surrounding_syntax_is_partial() {
    // The pinned grammar does not recognize GNU attributes on namespaces.
    // Check the actual introducer/list syntax, without relying on an ERROR shape.
    for introducer in ["__attribute__", "__attribute"] {
        for gap in ["", " /* comment */ ", "\r\n", " \\\n "] {
            let source = format!(
                "namespace {introducer}{gap}((visibility(\"default\"), deprecated(\"old\"))) named {{ void member(); }}\n\
                 void ordinary() {{ visibility(\"default\"); deprecated(\"old\"); ((alignment())); }}\n"
            );
            for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
                let facts = extract_file_with_mode(
                    &create_frontend(Language::Cpp).unwrap(),
                    FileId::generate("namespace.cpp"),
                    Path::new("namespace.cpp"),
                    &source,
                    "attributes",
                    mode,
                    &(),
                )
                .unwrap();
                let start = source.find("void ordinary").unwrap() as u32;
                let calls: Vec<_> = facts
                    .references
                    .iter()
                    .filter(|r| r.kind == ReferenceKind::Call)
                    .collect();
                assert_eq!(calls.len(), 3, "{source}\n{calls:#?}");
                assert!(calls.iter().all(|r| r.range.start_byte >= start));
                assert_eq!(facts.callsites.len(), 3);
            }
        }
    }
}
