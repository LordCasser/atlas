use super::*;

fn navigation(result: &CallContextResult) -> Vec<&CallContextItem> {
    result
        .items
        .iter()
        .filter(|i| matches!(i.role, "selected_callable" | "enclosing_callable"))
        .collect()
}

#[test]
fn callable_navigation_separates_prototypes_definitions_and_call_targets() {
    let source = "int leaf(int);\nint leaf(int x) { return x; }\nstruct Box { int read(); };\nint Box::read() { return leaf(7); }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    for (selection, expected, kind) in [
        ("leaf(int);", "leaf", ContextItemKind::Declaration),
        (
            "leaf(int x)",
            "int leaf(int x) { return x; }",
            ContextItemKind::Definition,
        ),
        ("read();", "read", ContextItemKind::Declaration),
        (
            "read() {",
            "int Box::read() { return leaf(7); }",
            ContextItemKind::Definition,
        ),
    ] {
        let start = source.find(selection).unwrap() as u32;
        let result = inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            start,
            start + 4,
            false,
            &|| false,
        )
        .unwrap();
        let items = navigation(&result);
        assert_eq!(items.len(), 1, "{result:?}");
        assert_eq!(items[0].role, "selected_callable");
        assert_eq!(items[0].kind, kind);
        assert_eq!(item_text(root.path(), &store, items[0]), expected);
        assert!(items[0].symbol_id.is_some());
        assert!(matches!(
            items[0].subject,
            ContextSubject::Region {
                symbol_id: None,
                ..
            }
        ));
    }
    let result = inspect(root.path(), &store, source, "leaf(7)");
    let items = navigation(&result);
    assert_eq!(items.len(), 1, "{result:?}");
    assert_eq!(items[0].role, "enclosing_callable");
    assert_eq!(
        item_text(root.path(), &store, items[0]),
        "int Box::read() { return leaf(7); }"
    );
}

#[test]
fn callable_navigation_handles_no_calls_comments_and_capture_evaluation_scopes() {
    let source = "int leaf();\nint outer(int seed) {\n// leaf in comment\nconst char* text = \"leaf in string\";\nauto cb = [value = seed + 3]() { auto inner = []() { return 9; }; return value + 4; };\nreturn seed + 5;\n}\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    for (selection, prefix) in [
        ("seed + 3", "int outer("),
        ("value + 4", "[value = seed + 3]"),
        ("return 9", "[]()"),
        ("seed + 5", "int outer("),
        ("leaf in comment", "int outer("),
        ("leaf in string", "int outer("),
    ] {
        let result = inspect(root.path(), &store, source, selection);
        let items = navigation(&result);
        assert_eq!(items.len(), 1, "{selection}: {result:?}");
        assert_eq!(items[0].role, "enclosing_callable");
        assert_eq!(items[0].kind, ContextItemKind::Definition);
        assert!(
            item_text(root.path(), &store, items[0]).starts_with(prefix),
            "{selection}: {items:?}"
        );
    }
}

#[test]
fn callable_navigation_does_not_guess_across_bodies_or_signature_regions() {
    let source = "namespace N { struct Box { int method(); }; }\nint first(int input = 8) { return input; }\nint second() { auto cb = []() { return 2; }; return 3; }\n";
    let (root, store) = fixture(&[("main.cpp", source)]);
    for selection in [
        "namespace N",
        "struct Box",
        "int input = 8",
        "input; }\nint second",
        "auto cb = []() { return 2; }; return 3",
    ] {
        let result = inspect(root.path(), &store, source, selection);
        assert!(navigation(&result).is_empty(), "{selection}: {result:?}");
        assert!(
            result
                .gaps
                .iter()
                .any(|g| g.code == "callable_navigation_unavailable")
        );
    }
}

#[test]
fn callable_navigation_retains_local_facts_when_other_source_is_broken() {
    let source = "#include \"absent.hpp\"\nint useful() { return 17; }\nvoid broken( {";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let file = store
        .find_files_by_path_prefix("main.cpp")
        .unwrap()
        .pop()
        .unwrap();
    let before_symbols = store.find_symbols_by_file(&file.file_id).unwrap();
    let before_refs = store.find_references_by_file(&file.file_id).unwrap();
    let result = inspect(root.path(), &store, source, "return 17");
    let items = navigation(&result);
    assert_eq!(items.len(), 1, "{result:?}");
    assert_eq!(
        item_text(root.path(), &store, items[0]),
        "int useful() { return 17; }"
    );
    let broken = inspect(root.path(), &store, source, "broken( {");
    assert!(navigation(&broken).is_empty(), "{broken:?}");
    assert_eq!(
        store.find_symbols_by_file(&file.file_id).unwrap().len(),
        before_symbols.len()
    );
    assert_eq!(
        store.find_references_by_file(&file.file_id).unwrap().len(),
        before_refs.len()
    );
    assert!(inspect_call_context(&store, root.path(), "main.cpp", 0, 1, false, &|| true).is_err());
}

#[test]
fn callable_navigation_does_not_promote_a_recovered_body_boundary() {
    for source in [
        "int broken() { return 1;",
        "int outer() { auto broken = []() { return 1;",
    ] {
        let (root, store) = fixture(&[("main.cpp", source)]);
        for selection in ["broken", "return 1"] {
            let result = inspect(root.path(), &store, source, selection);
            assert!(
                navigation(&result)
                    .iter()
                    .all(|item| item.kind != ContextItemKind::Definition),
                "{result:?}"
            );
            assert!(
                result
                    .gaps
                    .iter()
                    .any(|g| g.code == "callable_navigation_unavailable")
            );
        }
    }
    let source = "int broken() { return 1;";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let result = inspect(root.path(), &store, source, "broken");
    assert!(
        navigation(&result)
            .iter()
            .any(|item| item.kind == ContextItemKind::Declaration)
    );
    // Actual body delimiters preserve the scope despite an unrelated bad statement.
    for source in [
        "int usable() { @; return 2; }",
        "int usable() try { return 2; } catch (...) { return 0; }",
    ] {
        let (root, store) = fixture(&[("main.cpp", source)]);
        let result = inspect(root.path(), &store, source, "return 2");
        assert!(
            navigation(&result)
                .iter()
                .any(|item| item.kind == ContextItemKind::Definition),
            "{result:?}"
        );
    }
}
