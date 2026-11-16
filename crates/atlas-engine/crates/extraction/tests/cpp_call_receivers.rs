#![cfg(feature = "cpp")]

use std::path::Path;

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::{FileId, ReferenceKind, SymbolKind};

#[test]
fn allocation_regions_preserve_nested_calls_and_callable_ownership() {
    let source = r#"
struct Node { Node(int = 0); };
int value(); unsigned count(); void* place();
Node* global = new Node;
void run() {
    auto a = new Node(value());
    auto b = new Node[count()];
    auto c = new (place()) Node(value());
    auto d = new int(3);
    auto pending = [raw = new Node(1)]() { return new Node(2); };
}
"#;
    for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
        let facts = extract_file_with_mode(
            &create_frontend(types::Language::Cpp).unwrap(),
            FileId::generate("allocation.cpp"),
            Path::new("allocation.cpp"),
            source,
            "fixture",
            mode,
            &(),
        )
        .unwrap();
        let sites = &facts.cpp_types.as_ref().unwrap().allocation_sites;
        assert_eq!(sites.len(), 7, "{sites:?}");
        for (written, owner) in [
            ("new Node;", None),
            ("new Node(value())", Some("run")),
            ("new Node[count()]", Some("run")),
            ("new (place()) Node(value())", Some("run")),
            ("new int(3)", Some("run")),
            ("new Node(1)", Some("run")),
            ("new Node(2)", Some("<lambda@")),
        ] {
            let start = source.find(written).unwrap();
            let written = written.trim_end_matches(';');
            let site = sites
                .iter()
                .find(|s| s.range.start_byte as usize == start)
                .unwrap();
            assert_eq!(site.range.end_byte as usize, start + written.len());
            let actual = site
                .source_symbol
                .and_then(|id| facts.symbols.iter().find(|s| s.id == id));
            match owner {
                None => assert!(actual.is_none(), "{site:?}"),
                Some("<lambda@") => assert!(actual.unwrap().name.starts_with("<lambda@")),
                Some(name) => assert_eq!(actual.unwrap().qualified_name, name),
            }
        }
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call)
            .collect();
        assert_eq!(calls.iter().filter(|r| r.name == "value").count(), 2);
        assert_eq!(calls.iter().filter(|r| r.name == "count").count(), 1);
        assert_eq!(calls.iter().filter(|r| r.name == "place").count(), 1);
        assert!(
            !calls
                .iter()
                .any(|r| matches!(r.name.as_str(), "Node" | "int"))
        );
    }
}

#[test]
fn allocation_regions_do_not_turn_unevaluated_operands_or_text_into_execution_gaps() {
    let source = r#"
struct Node {};
void run() {
    auto a = sizeof(new Node);
    using P = decltype(new Node);
    bool b = noexcept(new Node);
    bool c = requires { new Node; };
    const char* text = "new Node";
    // new Node
}
"#;
    let facts = extract_file_with_mode(
        &create_frontend(types::Language::Cpp).unwrap(),
        FileId::generate("unevaluated.cpp"),
        Path::new("unevaluated.cpp"),
        source,
        "fixture",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    assert!(
        facts
            .cpp_types
            .as_ref()
            .unwrap()
            .allocation_sites
            .is_empty(),
        "{:?}",
        facts.cpp_types.as_ref().unwrap().allocation_sites
    );
}

#[test]
fn cpp_data_fields_and_pointer_returning_methods_remain_distinct() {
    let source = "class Owner { int count_; Device* device_; Device& ref_; Box<Device> wrapped_; int (*callback_)(int); Device* get(); void run(); };";
    let facts = extract_file_with_mode(
        &create_frontend(types::Language::Cpp).unwrap(),
        FileId::generate("fields.cpp"),
        Path::new("fields.cpp"),
        source,
        "fixture",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    for (name, expected) in [
        ("count_", SymbolKind::Field),
        ("device_", SymbolKind::Field),
        ("ref_", SymbolKind::Field),
        ("wrapped_", SymbolKind::Field),
        ("callback_", SymbolKind::Field),
        ("get", SymbolKind::Method),
        ("run", SymbolKind::Method),
    ] {
        let found: Vec<_> = facts.symbols.iter().filter(|s| s.name == name).collect();
        assert_eq!(found.len(), 1, "{name}: {found:#?}");
        assert_eq!(found[0].kind, expected, "{name}");
        assert_eq!(found[0].qualified_name, format!("Owner::{name}"));
        assert_eq!(
            &source[found[0].name_range.start_byte as usize..found[0].name_range.end_byte as usize],
            name
        );
    }
}

#[test]
fn cpp_calls_preserve_receivers_arguments_and_nested_calls() {
    let source = r#"
void run() {
    peer->run(1, nested(2));
    object.run(/* comment */ 3);
    this->run();
    ns::Utility::run(4);
    (factory())->run(5);
}
template<class... Args> void forward(Args... args) { target(args...); }
"#;
    let facts = extract_file_with_mode(
        &create_frontend(types::Language::Cpp).unwrap(),
        FileId::generate("calls.cpp"),
        Path::new("calls.cpp"),
        source,
        "fixture",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    for (text, receiver, arity) in [
        ("peer->run", Some("peer"), Some(2)),
        ("object.run", Some("object"), Some(1)),
        ("this->run", Some("this"), Some(0)),
        ("ns::Utility::run", Some("ns::Utility"), Some(1)),
        ("(factory())->run", Some("(factory())"), Some(1)),
        ("nested", None, Some(1)),
        ("factory", None, Some(0)),
        ("target", None, None),
    ] {
        let reference = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.text == text)
            .unwrap_or_else(|| panic!("missing call {text}: {:?}", facts.references));
        assert_eq!(reference.receiver.as_deref(), receiver, "{text}");
        assert_eq!(reference.arity, arity, "{text}");
        let callsite = facts
            .callsites
            .iter()
            .find(|c| c.reference_id == Some(reference.id))
            .unwrap();
        assert_eq!(callsite.receiver.as_deref(), receiver, "{text}");
        if let Some(arity) = arity {
            assert_eq!(callsite.args.len(), arity as usize, "{text}");
        }
        if text == "peer->run" {
            assert_eq!(callsite.args[1].value, "nested(2)");
        }
    }
}

#[test]
fn standalone_forward_types_have_identity_without_a_member_inventory() {
    let source = r#"
namespace api {
struct Device;
struct Holder {
    class Nested;
    struct Device* field;
};
struct Device* global;
class Annotation variable;
}
"#;
    for mode in [ExtractionMode::Structural, ExtractionMode::Full] {
        let facts = extract_file_with_mode(
            &create_frontend(types::Language::Cpp).unwrap(),
            FileId::generate("forward.cpp"),
            Path::new("forward.cpp"),
            source,
            "fixture",
            mode,
            &(),
        )
        .unwrap();
        let types = facts.cpp_types.as_ref().unwrap();
        for (qualified, identity) in [
            ("api::Device", true),
            ("api::Holder::Nested", true),
            ("api::Holder::Device", false),
            ("api::Annotation", false),
        ] {
            let symbol = facts
                .symbols
                .iter()
                .find(|s| s.qualified_name == qualified)
                .unwrap();
            let record = types
                .records
                .iter()
                .find(|r| r.symbol_id == symbol.id)
                .unwrap();
            assert!(!record.is_definition, "{qualified}");
            assert_eq!(record.identity_supported, identity, "{qualified}");
            assert!(!record.lookup_supported, "{qualified}");
        }
    }
}
