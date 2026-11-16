#![cfg(feature = "cpp")]

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use std::path::Path;
use types::{FileId, Language, ReferenceKind};

#[test]
fn closure_bodies_and_capture_initializers_keep_distinct_callers() {
    let source = r#"
int initialize(); void sink();
void entry() {
    auto outer = [value = initialize()] {
        auto inner = [] { sink(); };
        inner();
    };
    ([] { sink(); })();
}
"#;
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("closures.cpp"),
        Path::new("closures.cpp"),
        source,
        "test",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    let entry = facts.symbols.iter().find(|s| s.name == "entry").unwrap();
    let lambdas = &facts.cpp_types.as_ref().unwrap().lambda_captures;
    assert_eq!(lambdas.len(), 3);
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Call)
        .collect();
    assert_eq!(
        calls.len(),
        5,
        "include the parenthesized closure invocation"
    );
    assert_eq!(
        calls
            .iter()
            .find(|r| r.name == "initialize")
            .unwrap()
            .source_symbol,
        Some(entry.id)
    );
    assert_eq!(lambdas[0].enclosing_symbol, Some(entry.id));
    assert_eq!(lambdas[1].enclosing_symbol, lambdas[0].symbol_id);
    assert_eq!(lambdas[2].enclosing_symbol, Some(entry.id));
    assert_ne!(lambdas[0].symbol_id, lambdas[1].symbol_id);
    for call in calls.iter().filter(|r| r.name == "sink") {
        let body = lambdas
            .iter()
            .filter(|l| {
                l.body_range.start_byte <= call.range.start_byte
                    && call.range.end_byte <= l.body_range.end_byte
            })
            .min_by_key(|l| l.body_range.byte_len())
            .unwrap();
        assert_eq!(call.source_symbol, body.symbol_id);
        assert_ne!(call.source_symbol, Some(entry.id));
    }
    let invoke = calls
        .iter()
        .find(|r| r.name.starts_with("<lambda@"))
        .unwrap();
    assert_eq!(invoke.source_symbol, Some(entry.id));
    assert_eq!(invoke.arity, Some(0));
    assert_eq!(
        &source[invoke.range.start_byte as usize..invoke.range.end_byte as usize],
        "[]"
    );
}

#[test]
fn closure_identity_and_lexical_class_survive_extraction_modes() {
    let source = "namespace api { struct Worker { void run(); }; void Worker::run() { auto task = [this] { run(); }; } }";
    let mut expected = None;
    for mode in [
        ExtractionMode::Structural,
        ExtractionMode::Manifest,
        ExtractionMode::ResolutionSymbols,
        ExtractionMode::Full,
    ] {
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("closures.cpp"),
            Path::new("closures.cpp"),
            source,
            "test",
            mode.clone(),
            &(),
        )
        .unwrap();
        if matches!(mode, ExtractionMode::Manifest) {
            // Manifest intentionally contains only top-level declarations;
            // nested callable identity belongs to symbol/structural/full modes.
            assert!(!facts.symbols.iter().any(|s| s.name.starts_with("<lambda@")));
            continue;
        }
        let lambda = facts
            .symbols
            .iter()
            .find(|s| s.name.starts_with("<lambda@"))
            .unwrap();
        assert!(
            lambda.qualified_name.starts_with("api::Worker::<lambda@"),
            "{}",
            lambda.qualified_name
        );
        if let Some(id) = expected {
            assert_eq!(lambda.id, id);
        } else {
            expected = Some(lambda.id);
        }
    }
}
