#![cfg(all(feature = "cpp", feature = "java", feature = "kotlin"))]

use std::{collections::BTreeSet, path::Path};

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::{FileFacts, FileId, Language, SymbolKind};

fn extract(language: Language, source: &str) -> FileFacts {
    extract_mode(language, source, ExtractionMode::Structural)
}

fn extract_mode(language: Language, source: &str, mode: ExtractionMode) -> FileFacts {
    extract_file_with_mode(
        &create_frontend(language).unwrap(),
        FileId::generate("identity"),
        Path::new("identity"),
        source,
        "fixture",
        mode,
        &(),
    )
    .unwrap()
}

#[test]
fn identity_agrees_across_manifest_resolution_structural_and_full() {
    for (language, source) in [
        (
            Language::Cpp,
            "int run(int value) { return value; } int run(double value) { return 1; }",
        ),
        (
            Language::Java,
            "package demo; class Service { int run(int value) { return value; } int run(String value) { return 1; } }",
        ),
        (
            Language::Kotlin,
            "package demo\nfun run(value: Int): Int { return value }\nfun run(value: String): Int { return 1 }",
        ),
    ] {
        let structural = extract(language, source);
        for mode in [
            ExtractionMode::Manifest,
            ExtractionMode::ResolutionSymbols,
            ExtractionMode::Full,
        ] {
            let facts = extract_mode(language, source, mode);
            assert!(!facts.symbols.is_empty());
            for symbol in facts.symbols.iter().filter(|s| {
                matches!(
                    s.kind,
                    SymbolKind::Class | SymbolKind::Function | SymbolKind::Method
                )
            }) {
                assert!(
                    structural
                        .symbols
                        .iter()
                        .any(|s| s.id == symbol.id && s.qualified_name == symbol.qualified_name),
                    "{language:?}: {}",
                    symbol.qualified_name
                );
            }
        }
    }
}

#[test]
fn cpp_overloads_and_cv_ref_qualifiers_keep_distinct_bodies() {
    let source = r#"
namespace demo {
int Stub::run(int value) { return first(value); }
int Stub::run(double value) { return second(value); }
int Stub::read() { return mutableValue(); }
int Stub::read() const { return constantValue(); }
int Stub::take() & { return lvalue(); }
int Stub::take() && { return rvalue(); }
}
"#;
    let facts = extract(Language::Cpp, source);
    for name in ["demo::Stub::run", "demo::Stub::read", "demo::Stub::take"] {
        let symbols: Vec<_> = facts
            .symbols
            .iter()
            .filter(|s| s.qualified_name == name)
            .collect();
        assert_eq!(symbols.len(), 2, "{name}");
        assert_ne!(
            symbols[0].id, symbols[1].id,
            "{name} folded distinct bodies"
        );
        for symbol in symbols {
            assert!(
                facts
                    .references
                    .iter()
                    .any(|r| r.source_symbol == Some(symbol.id)),
                "{name}: calls must retain their containing overload"
            );
        }
    }
}

#[test]
fn java_packages_nested_owners_and_overloads_are_preserved() {
    let source = r#"
package com.example;
class Outer {
    class Inner {
        int run(int value) { return first(value); }
        int run(String value) { return second(value); }
    }
}
"#;
    let facts = extract(Language::Java, source);
    assert!(
        facts
            .symbols
            .iter()
            .any(|s| s.qualified_name == "com.example.Outer.Inner")
    );
    let methods: Vec<_> = facts.symbols.iter().filter(|s| s.name == "run").collect();
    assert_eq!(methods.len(), 2);
    assert!(
        methods
            .iter()
            .all(|s| s.qualified_name == "com.example.Outer.Inner.run")
    );
    assert_ne!(methods[0].id, methods[1].id);
    let other = extract(
        Language::Java,
        &source.replace("com.example", "org.example"),
    );
    assert_ne!(
        methods[0].id,
        other.symbols.iter().find(|s| s.name == "run").unwrap().id
    );
}

#[test]
fn kotlin_packages_receivers_defaults_and_nested_owners_are_preserved() {
    let source = r#"
package com.example
class Outer {
    class Inner {
        fun run(value: Int = 1): Int { return value }
        fun run(value: String = "x"): Int { return value.length }
    }
}
fun String.read(): Int { return length }
fun Int.read(): Int { return this }
"#;
    let facts = extract(Language::Kotlin, source);
    for (name, count) in [("com.example.Outer.Inner.run", 2), ("com.example.read", 2)] {
        let symbols: Vec<_> = facts
            .symbols
            .iter()
            .filter(|s| s.qualified_name == name)
            .collect();
        assert_eq!(symbols.len(), count, "missing {name}: {:?}", facts.symbols);
        assert_eq!(
            symbols.iter().map(|s| s.id).collect::<BTreeSet<_>>().len(),
            count
        );
    }
    assert!(
        facts
            .symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Package && s.qualified_name == "com.example")
    );
}

#[test]
fn callable_identity_ignores_body_comments_and_formatting() {
    for (language, before, after) in [
        (
            Language::Cpp,
            "int Stub::run(int value) { return 1; }",
            "\nint Stub::run( int /* note */ value ) { return 2; }",
        ),
        (
            Language::Java,
            "package demo; class Stub { int run(int value) { return 1; } }",
            "package demo; class Stub {\nint run( int /* note */ value ) { return 2; } }",
        ),
        (
            Language::Kotlin,
            "package demo\nfun run(value: Int): Int { return 1 }",
            "package demo\n\nfun run( value: Int /* note */ ): Int { return 2 }",
        ),
    ] {
        let before = extract(language, before);
        let after = extract(language, after);
        assert_eq!(
            before.symbols.iter().find(|s| s.name == "run").unwrap().id,
            after.symbols.iter().find(|s| s.name == "run").unwrap().id,
            "{language:?}"
        );
    }
}
