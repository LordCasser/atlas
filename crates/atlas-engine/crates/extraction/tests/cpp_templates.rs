#![cfg(feature = "cpp")]

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use std::path::Path;
use types::{FileFacts, FileId, Language, ReferenceKind};

fn extract(source: &str) -> FileFacts {
    extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("main.cpp"),
        Path::new("main.cpp"),
        source,
        "fixture",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap()
}

#[test]
fn template_calls_keep_lookup_spelling_arguments_and_original_reference_identity() {
    let source = r#"
template<class T> void fetch(T&);
namespace ns { template<class T> void fetch(T&); }
struct Box { template<class T> void fetch(T&); };
template<class T> auto factory() { return []{}; }
template<class Object> void invoke(Object* object, int& value) { object->template fetch<int>(value); }
void run(Box& box, int& value) {
    fetch<const unsigned int>(value); ns::fetch<int>(value); box.fetch<int>(value);
    (fetch<int>)(value); fetch<>(value); fetch<7>(value); fetch<int*>(value); factory<int>()();
    static_cast<int>(value);
}
"#;
    let facts = extract(source);
    let cpp = facts.cpp_types.as_ref().unwrap();
    for (written, name, text, receiver, args) in [
        (
            "fetch<const unsigned int>",
            "fetch",
            "fetch",
            None,
            Some("const unsigned int"),
        ),
        (
            "ns::fetch<int>",
            "fetch",
            "ns::fetch",
            Some("ns"),
            Some("int"),
        ),
        (
            "box.fetch<int>",
            "fetch",
            "box.fetch",
            Some("box"),
            Some("int"),
        ),
        (
            "object->template fetch<int>",
            "fetch",
            "object->fetch",
            Some("object"),
            Some("int"),
        ),
        ("(fetch<int>)", "fetch", "fetch", None, Some("int")),
        ("fetch<>", "fetch", "fetch", None, Some("")),
        ("fetch<7>", "fetch", "fetch", None, None),
        ("fetch<int*>", "fetch", "fetch", None, None),
    ] {
        let reference = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.text == written)
            .unwrap_or_else(|| panic!("missing {written}: {:#?}", facts.references));
        let call = cpp
            .template_calls
            .iter()
            .find(|c| c.reference_id == reference.id)
            .unwrap();
        assert_eq!(call.name, name);
        assert_eq!(call.text, text);
        assert_eq!(call.receiver.as_deref(), receiver);
        assert_eq!(
            &source[call.name_range.start_byte as usize..call.name_range.end_byte as usize],
            name
        );
        assert!(
            source
                [call.arguments_range.start_byte as usize..call.arguments_range.end_byte as usize]
                .starts_with('<')
        );
        match args {
            None => assert!(call.arguments.is_none()),
            Some("") => assert_eq!(call.arguments.as_deref(), Some([].as_slice())),
            Some(expected) => {
                let values = call.arguments.as_ref().unwrap();
                assert_eq!(values.len(), 1);
                assert_eq!(
                    format!(
                        "{}{}",
                        if values[0].const_ { "const " } else { "" },
                        values[0].name
                    ),
                    expected
                );
            }
        }
    }
    let factories: Vec<_> = cpp
        .template_calls
        .iter()
        .filter(|c| c.name == "factory")
        .collect();
    assert_eq!(
        factories.len(),
        1,
        "outer invocation must not borrow inner template identity"
    );
    assert!(!cpp.template_calls.iter().any(|c| c.name == "static_cast"));
}

#[test]
fn primary_template_signatures_retain_order_and_declared_parameter_types() {
    let source = r#"
template<class A, typename B> A convert(B& value);
template<class X, typename Y> X convert(Y& value) { return X{}; }
struct Reader { template<class T> bool read(T& value); template<class T> T read(); };
template<class U> bool Reader::read(U& value) { return true; }
void ordinary(int& value) {}
template<int N> void count(int);
template<class... T> void pack(T...);
template<class T = int> void defaults(T);
template<> void count<7>(int);
"#;
    let facts = extract(source);
    let cpp = facts.cpp_types.as_ref().unwrap();
    let signatures = |name: &str| {
        cpp.callables
            .iter()
            .filter(|c| {
                facts
                    .symbols
                    .iter()
                    .any(|s| s.id == c.symbol_id && s.qualified_name == name)
            })
            .collect::<Vec<_>>()
    };
    let converts = signatures("convert");
    assert_eq!(converts.len(), 2, "{cpp:#?}");
    assert_eq!(
        converts[0].template_parameters.as_deref(),
        Some(["A".to_string(), "B".to_string()].as_slice())
    );
    assert_eq!(
        converts[1].template_parameters.as_deref(),
        Some(["X".to_string(), "Y".to_string()].as_slice())
    );
    let readers = signatures("Reader::read");
    assert_eq!(readers.len(), 3, "{cpp:#?}");
    assert!(
        readers
            .iter()
            .all(|c| c.template_parameters.as_ref().unwrap().len() == 1)
    );
    assert_eq!(readers.iter().filter(|c| c.minimum_arity == 0).count(), 1);
    assert_eq!(
        readers
            .iter()
            .filter(|c| c
                .parameter_declared_types
                .first()
                .is_some_and(|t| t.as_ref().unwrap().reference))
            .count(),
        2
    );
    assert!(signatures("ordinary")[0].template_parameters.is_none());
    for name in ["count", "pack", "defaults"] {
        assert!(signatures(name).is_empty(), "{name}");
    }
}
