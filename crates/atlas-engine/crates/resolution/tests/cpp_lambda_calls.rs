use db::Store;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use resolution::ReferenceResolver;
use std::{path::Path, sync::Arc};
use types::{FileId, Language, ReferenceKind};

#[test]
fn closure_invocations_do_not_follow_definition_or_argument_containment() {
    let source = r#"
void sink() {}
void consume(void (*)()) {}
void unused() { auto task = [] { sink(); }; }
void invoked() { auto task = [] { sink(); }; task(); }
void immediate() { ([] { sink(); })(); }
void passed() { consume([] { sink(); }); }
void nested() { auto outer = [] { auto inner = [] { sink(); }; }; outer(); }
void wrong_arity() { [] (int value) { sink(); }(); }
void const_mutable() { const auto task = [] () mutable { sink(); }; task(); }
void ordinary_const() { const auto task = [] { sink(); }; task(); }
void uncaptured() { auto task = [] {}; auto caller = [] { task(); }; }
"#;
    let id = FileId::generate("closures.cpp");
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        id,
        Path::new("closures.cpp"),
        source,
        "test",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    store.insert_file_facts(&facts).unwrap();
    let (resolved, _) = ReferenceResolver::new(store)
        .resolve_for_files(&[id])
        .unwrap();
    let lambdas = &facts.cpp_types.as_ref().unwrap().lambda_captures;
    let lambda_ids: Vec<_> = lambdas.iter().filter_map(|l| l.symbol_id).collect();
    for (reference, target) in &resolved {
        if reference.kind == ReferenceKind::Call && reference.name == "sink" {
            assert!(lambda_ids.contains(&reference.source_symbol.unwrap()));
            assert_eq!(
                facts
                    .symbols
                    .iter()
                    .find(|s| s.id == target.symbol_id)
                    .unwrap()
                    .name,
                "sink"
            );
        }
    }
    for (name, expected) in [
        ("unused", 0),
        ("invoked", 1),
        ("immediate", 1),
        ("passed", 0),
        ("nested", 1),
        ("wrong_arity", 0),
        ("const_mutable", 0),
        ("ordinary_const", 1),
        ("uncaptured", 0),
    ] {
        let owner = facts.symbols.iter().find(|s| s.name == name).unwrap();
        let edges: Vec<_> = resolved
            .iter()
            .filter(|(r, t)| {
                r.kind == ReferenceKind::Call
                    && r.source_symbol == Some(owner.id)
                    && lambda_ids.contains(&t.symbol_id)
            })
            .collect();
        assert_eq!(edges.len(), expected, "{name}");
    }
    let unbound = facts
        .references
        .iter()
        .find(|r| {
            r.kind == ReferenceKind::Call
                && r.name == "task"
                && r.range.start_byte as usize > source.find("void uncaptured").unwrap()
        })
        .unwrap();
    assert!(!resolved.iter().any(|(r, _)| r.id == unbound.id));
}
