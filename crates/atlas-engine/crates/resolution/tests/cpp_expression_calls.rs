use db::Store;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use resolution::ReferenceResolver;
use std::{path::Path, sync::Arc};
use types::{FileId, Language, ReferenceKind};

#[test]
fn returned_callable_is_not_resolved_as_its_getter_or_contained_sink() {
    let source = r#"
struct Action { void operator()() {} };
Action make() { return Action{}; }
void sink() {}
void getter_only() { make(); }
void returned() { make()(); }
void indirect() { (make())(); }
void closure() { ([] { sink(); })(); }
void pending() { auto task = [] { sink(); }; }
"#;
    let id = FileId::generate("expressions.cpp");
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        id,
        Path::new("expressions.cpp"),
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
    let make = facts.symbols.iter().find(|s| s.name == "make").unwrap();
    for name in ["getter_only", "returned", "indirect"] {
        let owner = facts.symbols.iter().find(|s| s.name == name).unwrap();
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call && r.source_symbol == Some(owner.id))
            .collect();
        assert_eq!(calls.len(), if name == "getter_only" { 1 } else { 2 });
        let edges: Vec<_> = resolved
            .iter()
            .filter(|(r, _)| r.kind == ReferenceKind::Call && r.source_symbol == Some(owner.id))
            .collect();
        assert_eq!(
            edges.len(),
            1,
            "known getter stays useful without guessing its returned callable"
        );
        assert_eq!(edges[0].0.name, "make");
        assert_eq!(edges[0].1.symbol_id, make.id);
        assert!(
            calls
                .iter()
                .filter(|r| r.name != "make")
                .all(|r| !resolved.iter().any(|(known, _)| known.id == r.id))
        );
    }
    let closure = facts.symbols.iter().find(|s| s.name == "closure").unwrap();
    assert_eq!(
        resolved
            .iter()
            .filter(|(r, _)| r.kind == ReferenceKind::Call && r.source_symbol == Some(closure.id))
            .count(),
        1
    );
    let pending = facts.symbols.iter().find(|s| s.name == "pending").unwrap();
    assert!(
        !resolved
            .iter()
            .any(|(r, _)| r.kind == ReferenceKind::Call && r.source_symbol == Some(pending.id))
    );
}
