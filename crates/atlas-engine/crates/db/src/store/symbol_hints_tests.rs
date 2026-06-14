//! Tests for symbol_hints store module.

use super::symbol_hints::SymbolHint;
use super::*;

fn test_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    store
}

fn make_file_id_bytes(name: &str) -> Vec<u8> {
    types::ids::FileId::generate(name).as_bytes().to_vec()
}

#[test]
fn test_insert_and_query_hint() {
    let store = test_store();
    let file_id = make_file_id_bytes("src/main.rs");

    store
        .insert_symbol_hint("my_function", &file_id, "function", 42, 0.9, "manifest")
        .unwrap();

    let hints = store.query_symbol_hints("my_function").unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].name, "my_function");
    assert_eq!(hints[0].kind, "function");
    assert_eq!(hints[0].line, 42);
    assert_eq!(hints[0].confidence, 0.9);
    assert_eq!(hints[0].source, "manifest");
}

#[test]
fn test_case_insensitive_query() {
    let store = test_store();
    let file_id = make_file_id_bytes("src/lib.rs");

    store
        .insert_symbol_hint("FooBar", &file_id, "struct", 10, 0.8, "manifest")
        .unwrap();

    // Query with lowercase should match
    let hints = store.query_symbol_hints("foobar").unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].name, "FooBar");

    // Query with mixed case should also match
    let hints = store.query_symbol_hints("FOOBAR").unwrap();
    assert_eq!(hints.len(), 1);
}

#[test]
fn test_has_hints_true() {
    let store = test_store();
    let file_id = make_file_id_bytes("src/main.rs");

    store
        .insert_symbol_hint("exists", &file_id, "function", 1, 0.9, "manifest")
        .unwrap();

    assert!(store.has_symbol_hints("exists").unwrap());
}

#[test]
fn test_has_hints_false() {
    let store = test_store();

    assert!(!store.has_symbol_hints("nonexistent").unwrap());
}

#[test]
fn test_batch_insert_hints() {
    let store = test_store();
    let f1 = make_file_id_bytes("a.ts");
    let f2 = make_file_id_bytes("b.ts");

    let hints = vec![
        SymbolHint {
            name: "alpha".into(),
            file_id: f1.clone(),
            kind: "function".into(),
            line: 1,
            confidence: 0.9,
            source: "manifest".into(),
            freshness: String::new(),
        },
        SymbolHint {
            name: "beta".into(),
            file_id: f1.clone(),
            kind: "class".into(),
            line: 10,
            confidence: 0.8,
            source: "manifest".into(),
            freshness: String::new(),
        },
        SymbolHint {
            name: "gamma".into(),
            file_id: f2.clone(),
            kind: "function".into(),
            line: 5,
            confidence: 0.7,
            source: "manifest".into(),
            freshness: String::new(),
        },
    ];

    let count = store.insert_symbol_hints_batch(&hints).unwrap();
    assert_eq!(count, 3);

    let alpha_hints = store.query_symbol_hints("alpha").unwrap();
    assert_eq!(alpha_hints.len(), 1);

    let gamma_hints = store.query_symbol_hints("gamma").unwrap();
    assert_eq!(gamma_hints.len(), 1);
}

#[test]
fn test_query_returns_empty_for_unknown_name() {
    let store = test_store();

    let hints = store.query_symbol_hints("definitely_not_there").unwrap();
    assert!(hints.is_empty());
}

#[test]
fn test_insert_or_replace_hint() {
    let store = test_store();
    let file_id = make_file_id_bytes("src/main.rs");

    // Insert first value
    store
        .insert_symbol_hint("duplicate", &file_id, "function", 10, 0.5, "manifest")
        .unwrap();

    // Replace with different confidence (same PK name+file_id)
    store
        .insert_symbol_hint("duplicate", &file_id, "function", 10, 1.0, "manifest")
        .unwrap();

    let hints = store.query_symbol_hints("duplicate").unwrap();
    assert_eq!(hints.len(), 1);
    assert!((hints[0].confidence - 1.0).abs() < f64::EPSILON);
}

// ── C13: has_symbol_hints is case-insensitive ───────────────────────────────

#[test]
fn test_has_hints_case_insensitive() {
    let store = test_store();
    let file_id = make_file_id_bytes("src/case_test.rs");

    store
        .insert_symbol_hint("CaseSensitive", &file_id, "function", 1, 0.9, "manifest")
        .unwrap();

    // Exact case
    assert!(store.has_symbol_hints("CaseSensitive").unwrap());

    // Lowercase
    assert!(store.has_symbol_hints("casesensitive").unwrap());

    // Uppercase
    assert!(store.has_symbol_hints("CASESENSITIVE").unwrap());

    // Different case should NOT exist
    assert!(!store.has_symbol_hints("SomethingElse").unwrap());
}

// ── C14: Batch insert is idempotent (INSERT OR REPLACE) ─────────────────────

#[test]
fn test_batch_insert_idempotent() {
    let store = test_store();
    let f1 = make_file_id_bytes("idem_a.ts");
    let f2 = make_file_id_bytes("idem_b.ts");

    let hints = vec![
        SymbolHint {
            name: "idem_alpha".into(),
            file_id: f1.clone(),
            kind: "function".into(),
            line: 1,
            confidence: 0.9,
            source: "manifest".into(),
            freshness: String::new(),
        },
        SymbolHint {
            name: "idem_beta".into(),
            file_id: f2.clone(),
            kind: "class".into(),
            line: 10,
            confidence: 0.8,
            source: "manifest".into(),
            freshness: String::new(),
        },
    ];

    // First insert
    let count1 = store.insert_symbol_hints_batch(&hints).unwrap();
    assert_eq!(count1, 2);

    // Second insert — same data, should be idempotent (INSERT OR REPLACE)
    let count2 = store.insert_symbol_hints_batch(&hints).unwrap();
    assert_eq!(count2, 2);

    // Only 2 rows exist (no duplicates from second insert)
    let alpha_hints = store.query_symbol_hints("idem_alpha").unwrap();
    assert_eq!(alpha_hints.len(), 1);

    let beta_hints = store.query_symbol_hints("idem_beta").unwrap();
    assert_eq!(beta_hints.len(), 1);
}

// ── C15: Multiple files with the same symbol name ───────────────────────────

#[test]
fn test_multiple_files_same_symbol_name() {
    let store = test_store();
    let f1 = make_file_id_bytes("src/file_a.rs");
    let f2 = make_file_id_bytes("src/file_b.rs");
    let f3 = make_file_id_bytes("src/file_c.rs");

    // Same name "common_func" in 3 different files
    store
        .insert_symbol_hint("common_func", &f1, "function", 10, 0.9, "manifest")
        .unwrap();
    store
        .insert_symbol_hint("common_func", &f2, "function", 42, 0.7, "manifest")
        .unwrap();
    store
        .insert_symbol_hint("common_func", &f3, "function", 5, 0.5, "manifest")
        .unwrap();

    let hints = store.query_symbol_hints("common_func").unwrap();
    assert_eq!(hints.len(), 3);

    // Sorted by confidence descending
    let confidences: Vec<f64> = hints.iter().map(|h| h.confidence).collect();
    let mut sorted = confidences.clone();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    assert_eq!(
        confidences, sorted,
        "hints should be ordered by confidence descending"
    );

    // Verify all 3 have the same name
    for hint in &hints {
        assert_eq!(hint.name, "common_func");
    }

    // has_symbol_hints returns true
    assert!(store.has_symbol_hints("common_func").unwrap());
}
