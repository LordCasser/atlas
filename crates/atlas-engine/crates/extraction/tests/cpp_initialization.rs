#![cfg(feature = "cpp")]

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use std::{collections::BTreeMap, path::Path};
use types::{
    FileFacts, FileId, Language, ReferenceKind,
    lazy::{AnalysisUnit, LazyWindow},
};

fn extract(source: &str, mode: ExtractionMode) -> FileFacts {
    extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("init.cpp"),
        Path::new("init.cpp"),
        source,
        "fixed",
        mode,
        &(),
    )
    .unwrap()
}

// These expectations were source-reviewed before the parser change. The
// compiler observations are retained separately and do not define this oracle.
#[test]
fn nested_initializers_and_genuine_declarations_keep_distinct_meanings() {
    for (label, source, expected) in [
        (
            "two_values",
            r###"struct Status { explicit Status(int); };
int build(int, int);
void run() { int x = 1; int y = 2; Status value(build(x, y)); }
"###,
            Some("build(x, y)"),
        ),
        (
            "two_literals",
            r###"struct Status { explicit Status(int); };
int build(int, int);
void run() { int x = 1; int y = 2; Status value(build(1, 2)); }
"###,
            Some("build(1, 2)"),
        ),
        (
            "parenthesized_expression",
            r###"struct Status { explicit Status(int); };
int build(int, int);
void run() { int x = 1; int y = 2; Status value((build(x, y))); }
"###,
            Some("build(x, y)"),
        ),
        (
            "binary_argument",
            r###"struct Status { explicit Status(int); };
int build(int, int);
void run() { int x = 1; int y = 2; Status value(build(x + 1, y)); }
"###,
            Some("build(x + 1, y)"),
        ),
        (
            "braced_control",
            r###"struct Status { explicit Status(int); };
int build(int, int);
void run() { int x = 1; int y = 2; Status value{build(x, y)}; }
"###,
            Some("build(x, y)"),
        ),
        (
            "qualified_function",
            r###"struct Status { explicit Status(int); };
namespace ops { int build(int, int); }
void run() { int x = 1; int y = 2; Status value(ops::build(x, y)); }
"###,
            Some("ops::build(x, y)"),
        ),
        (
            "zero_arguments",
            r###"struct Status { explicit Status(int); };
int build();
void run() {  Status value(build()); }
"###,
            Some("build()"),
        ),
        (
            "one_value",
            r###"struct Status { explicit Status(int); };
int build(int);
void run() { int x = 1; Status value(build(x)); }
"###,
            Some("build(x)"),
        ),
        (
            "builtin_type",
            r###"struct Status { explicit Status(int); };

void run() { int x = 1; Status value(int(x)); }
"###,
            None,
        ),
        (
            "alias_type",
            r###"struct Status { explicit Status(int); };
using Build = int;
void run() { int x = 1; Status value(Build(x)); }
"###,
            None,
        ),
        (
            "function_type_parameter",
            r###"struct Status { explicit Status(int); };
using Build = int; using X = int; using Y = int;
void run() {  Status value(Build(X, Y)); }
"###,
            None,
        ),
        (
            "type_shadowing",
            r###"struct Status { explicit Status(int); };
int Build(int);
void run() { int x = 1; using Build = int; Status value(Build(x)); }
"###,
            None,
        ),
        (
            "value_shadowing",
            r###"struct Status { explicit Status(int); };
using Build = int; int make(int);
void run() { int x = 1; auto Build = &make; Status value(Build(x)); }
"###,
            Some("Build(x)"),
        ),
        (
            "nested_type_shadowing",
            r###"struct Status { explicit Status(int); };
using Build = int;
void run() { int X = 1; int Y = 2; { using X = int; using Y = int; Status value(Build(X, Y)); } }
"###,
            None,
        ),
        (
            "inherited_member",
            r###"struct Status { explicit Status(int); };
struct Base { int build(int, int); }; struct Worker : Base { void run(); };
void Worker::run() { int x = 1; int y = 2; Status value(build(x, y)); }
"###,
            Some("build(x, y)"),
        ),
    ] {
        let start = source.find("Status value").unwrap();
        let end = start + source[start..].find(';').unwrap() + 1;
        for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
            let facts = extract(source, mode);
            let references: Vec<_> = facts
                .references
                .iter()
                .filter(|r| {
                    r.kind == ReferenceKind::Call
                        && r.range.start_byte as usize >= start
                        && r.range.end_byte as usize <= end
                })
                .collect();
            assert_eq!(references.len(), usize::from(expected.is_some()), "{label}");
            if let Some(expected) = expected {
                let reference = references[0];
                let call = facts
                    .callsites
                    .iter()
                    .find(|c| c.reference_id == Some(reference.id))
                    .unwrap();
                assert_eq!(
                    &source[call.range.start_byte as usize..call.range.end_byte as usize],
                    expected,
                    "{label}"
                );
                assert_eq!(
                    &source[reference.range.start_byte as usize..reference.range.end_byte as usize],
                    reference.name
                );
                let owner = facts
                    .symbols
                    .iter()
                    .find(|s| Some(s.id) == reference.source_symbol)
                    .unwrap();
                assert_eq!(owner.name, "run", "{label}");
                for arg in &call.args {
                    let range = arg.range.unwrap();
                    assert_eq!(
                        &source[range.start_byte as usize..range.end_byte as usize],
                        arg.value,
                        "{label}"
                    );
                }
                assert!(
                    facts
                        .bindings
                        .iter()
                        .any(|b| b.name == "value" && b.range.start_byte as usize >= start)
                );
            }
        }
    }
}

#[test]
fn lookup_does_not_bypass_type_shadowing_or_unavailable_scopes() {
    for source in [
        "struct Status { Status(int); }; int build(int); struct Holder { using build = int; void run() { int x = 1; Status value(build(x)); } };",
        "struct Status { Status(int); }; int build(int); struct Holder { using build = int; void run(); }; void Holder::run() { int x = 1; Status value(build(x)); }",
        "struct Status { Status(int); }; int build(int); template<class build> void run() { int x = 1; Status value(build(x)); }",
        "struct Status { Status(int); }; int build(int); void run() { int x = 1; struct build {}; Status value(build(x)); }",
        "struct Status { Status(int); }; int build(int);\n#define ALIAS(name) using name = int\nvoid run() { int x = 1; ALIAS(build); Status value(build(x)); }",
        "struct Status { Status(int); }; using Build = int; void run() { { int X = 1; int Y = 2; } Status value(Build(X, Y)); }",
        "struct Status { Status(int); }; using Build = int; using X = int; using Y = int; void run() { Status value(Build(X, Y)); int X = 1; int Y = 2; }",
        "struct Status { Status(int); }; using Y = int; void run() { Unknown * x; Status value(Build(x, Y)); }",
        "struct Status { Status(int); }; struct X { operator int() const { return 0; } }; struct Y {}; using Build = int; int low = 1, high = 2; void run() { low < high > X(); Status value(Build(X, Y)); }",
    ] {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let original = parser.parse(source, None).unwrap();
        let original_shape = original.root_node().to_sexp();
        let frontend = create_frontend(Language::Cpp).unwrap();
        let refined = frontend
            .parser
            .refine_tree(source, original, &|| false)
            .unwrap();
        assert_eq!(refined.root_node().to_sexp(), original_shape, "{source}");
        let facts = extract(source, ExtractionMode::Structural);
        let start = source.find("Status value").unwrap();
        assert!(
            !facts
                .references
                .iter()
                .any(|r| r.kind == ReferenceKind::Call && r.range.start_byte as usize >= start),
            "{source}"
        );
    }
}

#[test]
fn coordinates_and_direct_initialization_survive_multiple_expression_hints() {
    let source = "// 原始字节\r\nstruct Status { explicit Status(int); };\r\nint build(int, int);\r\nvoid run() {\r\n int x = 1, y = 2;\r\n Status first(build(/* 保留 */ x, y));\r\n Status second(build(x, y));\r\n}\r\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .unwrap();
    let original = parser.parse(source, None).unwrap();
    // Annotation-aware indexing and inspection delegate to the same refiner.
    let frontend = extraction::cpp_annotations::frontend(vec![], vec![]).unwrap();
    let refined = frontend
        .parser
        .refine_tree(source, original, &|| false)
        .unwrap();
    assert_eq!(refined.root_node().end_byte(), source.len());
    assert!(!refined.root_node().has_error());
    for expected in ["build(/* 保留 */ x, y)", "build(x, y)"] {
        let start = source.find(expected).unwrap();
        let end = start + expected.len();
        let selected = refined
            .root_node()
            .descendant_for_byte_range(start, end)
            .unwrap();
        assert_eq!(selected.kind(), "call_expression");
        assert_eq!(
            selected.start_position().row,
            source[..start].bytes().filter(|b| *b == b'\n').count()
        );
        assert_eq!(
            selected.start_position().column,
            source[..start].rsplit('\n').next().unwrap().len()
        );
        let declaration = std::iter::successors(Some(selected), |n| n.parent())
            .find(|n| n.kind() == "init_declarator")
            .unwrap();
        let value = declaration.child_by_field_name("value").unwrap();
        assert_eq!(
            value.kind(),
            "argument_list",
            "must retain direct initialization"
        );
        assert_eq!(&source[value.start_byte()..value.start_byte() + 1], "(");
    }
    let shape = refined.root_node().to_sexp();
    let repeated = frontend
        .parser
        .refine_tree(source, refined, &|| false)
        .unwrap();
    assert_eq!(repeated.root_node().to_sexp(), shape);
    let facts = extract(source, ExtractionMode::Full);
    let calls: Vec<_> = facts
        .callsites
        .iter()
        .filter(|c| c.args.len() == 2)
        .collect();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(
            call.args
                .iter()
                .map(|a| a.value.as_str())
                .collect::<Vec<_>>(),
            ["x", "y"]
        );
    }
}

#[test]
fn full_and_lazy_keep_the_same_recovered_invocation_and_value_roles() {
    let source = "struct Status { explicit Status(int); }; int build(int x, int y) { return x + y; } void run() { int x = 1; int y = 2; Status value(build(x, y)); }";
    let structural = extract(source, ExtractionMode::Structural);
    let full = extract(source, ExtractionMode::Full);
    let function = structural.symbols.iter().find(|s| s.name == "run").unwrap();
    let unit =
        AnalysisUnit::from_function(FileId::generate("init.cpp"), function.id, function.range);
    let lazy = extract(
        source,
        ExtractionMode::LazyDataflow {
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
            callsites: structural.callsites.clone(),
        },
    );
    let full_nodes: BTreeMap<_, _> = full
        .data_nodes
        .iter()
        .filter(|n| n.function_id == Some(function.id))
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
    assert_eq!(structural.callsites.len(), 1);
    assert!(
        lazy_nodes
            .values()
            .any(|n| n.kind == types::DataNodeKind::CallReturn
                && n.callsite_id == Some(structural.callsites[0].id))
    );
}

#[test]
fn refinement_observes_cancellation() {
    let source =
        "struct Status { Status(int); }; int build(); void run() { Status value(build()); }";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let frontend = create_frontend(Language::Cpp).unwrap();
    assert!(
        frontend
            .parser
            .refine_tree(source, tree, &|| true)
            .is_none()
    );
}
