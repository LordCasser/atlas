//! Tests for closure_coverage store module.

use super::*;
use types::ids::FileId;

fn test_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    store
}

fn make_test_file_id(name: &str) -> Vec<u8> {
    FileId::generate(name).as_bytes().to_vec()
}

#[test]
fn test_insert_and_make_visible() {
    let store = test_store();
    let file_id = make_test_file_id("src/main.c");

    // Insert a closure first (FK constraint)
    store.insert_closure_generation("cl_cov_1").unwrap();

    // Insert staged coverage
    store
        .insert_closure_coverage(
            "cl_cov_1",
            &file_id,
            "extracted_structural",
            1,
            Some("abc123"),
        )
        .unwrap();

    // Verify not yet visible
    let visible = store.get_visible_coverage("cl_cov_1").unwrap();
    assert!(visible.is_empty());

    // Make visible
    let updated = store.make_coverage_visible("cl_cov_1", 1).unwrap();
    assert_eq!(updated, 1);

    // Verify now visible
    let visible = store.get_visible_coverage("cl_cov_1").unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].visibility_state, "visible");
    assert_eq!(visible[0].source, "extracted_structural");
}

#[test]
fn test_is_file_covered() {
    let store = test_store();
    let file_id = make_test_file_id("src/covered.ts");

    store.insert_closure_generation("cl_cov_2").unwrap();
    store
        .insert_closure_coverage("cl_cov_2", &file_id, "extracted_manifest", 1, None)
        .unwrap();

    // Not yet visible
    assert!(!store.is_file_covered(&file_id).unwrap());

    // Make visible
    store.make_coverage_visible("cl_cov_2", 1).unwrap();

    // Now covered
    assert!(store.is_file_covered(&file_id).unwrap());

    // Unrelated file
    let other = make_test_file_id("src/other.ts");
    assert!(!store.is_file_covered(&other).unwrap());
}

#[test]
fn test_get_coverage_counts() {
    let store = test_store();
    let file_a = make_test_file_id("src/a.rs");
    let file_b = make_test_file_id("src/b.rs");

    store.insert_closure_generation("cl_counts").unwrap();

    // Insert entries with different sources across different generations
    store
        .insert_closure_coverage(
            "cl_counts",
            &file_a,
            "extracted_structural",
            1,
            Some("hash1"),
        )
        .unwrap();
    store
        .insert_closure_coverage("cl_counts", &file_b, "extracted_manifest", 1, Some("hash2"))
        .unwrap();
    // Same file, different generation for manifest source
    store
        .insert_closure_coverage("cl_counts", &file_a, "extracted_manifest", 2, Some("hash3"))
        .unwrap();

    store.make_coverage_visible("cl_counts", 1).unwrap();
    store.make_coverage_visible("cl_counts", 2).unwrap();

    let counts = store.get_coverage_counts("cl_counts").unwrap();
    // 1 structural (gen 1) + 1 manifest (gen 1) + 1 manifest (gen 2) = 3 total
    let total: i64 = counts.iter().map(|(_, c)| c).sum();
    assert_eq!(total, 3);
}

// ── B9: Staged → visible transition with batch isolation ────────────────────

#[test]
fn test_staged_to_visible_transition() {
    let store = test_store();
    let f1 = make_test_file_id("src/trans_a.ts");
    let f2 = make_test_file_id("src/trans_b.ts");

    store.insert_closure_generation("cl_transition").unwrap();

    // Insert two staged entries
    store
        .insert_closure_coverage("cl_transition", &f1, "extracted_structural", 1, Some("h1"))
        .unwrap();
    store
        .insert_closure_coverage("cl_transition", &f2, "extracted_manifest", 1, Some("h2"))
        .unwrap();

    // Both are staged — not visible
    let visible = store.get_visible_coverage("cl_transition").unwrap();
    assert!(visible.is_empty(), "staged entries should not be visible");

    assert!(!store.is_file_covered(&f1).unwrap());
    assert!(!store.is_file_covered(&f2).unwrap());

    // Make generation 1 visible
    let updated = store.make_coverage_visible("cl_transition", 1).unwrap();
    assert_eq!(updated, 2);

    // Both now visible
    let visible = store.get_visible_coverage("cl_transition").unwrap();
    assert_eq!(visible.len(), 2);
    for v in &visible {
        assert_eq!(v.visibility_state, "visible");
    }
    assert!(store.is_file_covered(&f1).unwrap());
    assert!(store.is_file_covered(&f2).unwrap());

    // make_coverage_visible is idempotent — no staged entries to transition
    let second = store.make_coverage_visible("cl_transition", 1).unwrap();
    assert_eq!(second, 0);
}

// ── B10: Multiple closures covering the same file ───────────────────────────

#[test]
fn test_multiple_closures_same_file() {
    let store = test_store();
    let shared_file = make_test_file_id("src/shared.ts");

    // Closure 1
    store.insert_closure_generation("cl_multi_a").unwrap();
    store
        .insert_closure_coverage(
            "cl_multi_a",
            &shared_file,
            "extracted_structural",
            1,
            Some("hash_a"),
        )
        .unwrap();
    store.make_coverage_visible("cl_multi_a", 1).unwrap();

    // Closure 2 — same file
    store.insert_closure_generation("cl_multi_b").unwrap();
    store
        .insert_closure_coverage(
            "cl_multi_b",
            &shared_file,
            "extracted_manifest",
            1,
            Some("hash_b"),
        )
        .unwrap();
    store.make_coverage_visible("cl_multi_b", 1).unwrap();

    // Both closures have visible coverage for the shared file
    let a_visible = store.get_visible_coverage("cl_multi_a").unwrap();
    assert_eq!(a_visible.len(), 1);
    assert_eq!(a_visible[0].closure_id, "cl_multi_a");

    let b_visible = store.get_visible_coverage("cl_multi_b").unwrap();
    assert_eq!(b_visible.len(), 1);
    assert_eq!(b_visible[0].closure_id, "cl_multi_b");

    // The file is covered by some closure
    assert!(store.is_file_covered(&shared_file).unwrap());
}

// ── B11: Coverage counts grouped by source ──────────────────────────────────

#[test]
fn test_coverage_counts_by_source() {
    let store = test_store();
    let f1 = make_test_file_id("src/s1.rs");
    let f2 = make_test_file_id("src/s2.rs");
    let f3 = make_test_file_id("src/s3.rs");

    store.insert_closure_generation("cl_source_counts").unwrap();

    // 2 extracted_structural + 1 extracted_manifest
    store
        .insert_closure_coverage(
            "cl_source_counts",
            &f1,
            "extracted_structural",
            1,
            Some("h1"),
        )
        .unwrap();
    store
        .insert_closure_coverage(
            "cl_source_counts",
            &f2,
            "extracted_structural",
            1,
            Some("h2"),
        )
        .unwrap();

    store
        .insert_closure_coverage("cl_source_counts", &f3, "extracted_manifest", 1, Some("h3"))
        .unwrap();

    store.make_coverage_visible("cl_source_counts", 1).unwrap();

    let counts = store.get_coverage_counts("cl_source_counts").unwrap();
    assert_eq!(counts.len(), 2, "should have 2 source groups");

    let structural_count = counts
        .iter()
        .find(|(src, _)| src == "extracted_structural")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(structural_count, 2);

    let manifest_count = counts
        .iter()
        .find(|(src, _)| src == "extracted_manifest")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(manifest_count, 1);
}

// ── B12: Generation isolation — making gen 1 visible ≠ gen 2 visible ────────

#[test]
fn test_make_visible_generation_isolation() {
    let store = test_store();
    let f1 = make_test_file_id("src/iso_a.ts");
    let f2 = make_test_file_id("src/iso_b.ts");

    store.insert_closure_generation("cl_iso").unwrap();

    // Generation 1 entry
    store
        .insert_closure_coverage("cl_iso", &f1, "extracted_structural", 1, Some("g1"))
        .unwrap();

    // Generation 2 entry (same closure, different generation)
    store
        .insert_closure_coverage("cl_iso", &f2, "extracted_manifest", 2, Some("g2"))
        .unwrap();

    // Only make generation 1 visible
    let updated = store.make_coverage_visible("cl_iso", 1).unwrap();
    assert_eq!(updated, 1);

    // Gen 1 entry is visible
    let visible = store.get_visible_coverage("cl_iso").unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].generation, 1);
    assert_eq!(visible[0].visibility_state, "visible");

    // Gen 2 entry is still staged (not visible)
    let all_gen2: Vec<super::closure_coverage::ClosureCoverage> = {
        let conn = store.lock_read();
        let mut stmt = conn
            .prepare(
                "SELECT closure_id, file_id, source, visibility_state, generation,
                        content_hash, extracted_at
                 FROM closure_coverage
                 WHERE closure_id = ?1 AND generation = 2",
            )
            .unwrap();
        stmt.query_map(rusqlite::params!["cl_iso"], |row| {
            Ok(super::closure_coverage::ClosureCoverage {
                closure_id: row.get(0)?,
                file_id: row.get(1)?,
                source: row.get(2)?,
                visibility_state: row.get(3)?,
                generation: row.get(4)?,
                content_hash: row.get(5)?,
                extracted_at: row.get(6)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    };
    assert_eq!(all_gen2.len(), 1);
    assert_eq!(all_gen2[0].visibility_state, "staged");
}
