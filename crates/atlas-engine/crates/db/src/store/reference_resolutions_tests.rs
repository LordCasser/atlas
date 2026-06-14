//! Tests for reference_resolutions store module.

use super::*;
use types::ReferenceKind;
use types::ids::{FileId, ReferenceId};

fn test_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    store
}

fn make_ref_id(name: &str) -> Vec<u8> {
    let file_id = FileId::generate("test_file.ts");
    ReferenceId::generate(&file_id, None, 0, 10, name, ReferenceKind::Usage)
        .as_bytes()
        .to_vec()
}

// ── A1: Insert and get visible resolution ───────────────────────────────────

#[test]
fn test_insert_and_get_visible_resolution() {
    let store = test_store();
    let ref_id = make_ref_id("my_function");

    store.insert_closure_generation("cl_res_1").unwrap();

    // Insert staged resolution
    store
        .insert_reference_resolution(
            &ref_id,
            "cl_res_1",
            1,
            "closure_reachable",
            None,
            "closure_complete",
            "high",
            "closure_reachable",
            Some("import_based"),
        )
        .unwrap();

    // Not yet visible
    let visible = store.get_visible_resolution(&ref_id, "cl_res_1").unwrap();
    assert!(visible.is_empty());

    // Make visible
    let updated = store.make_resolutions_visible("cl_res_1", 1).unwrap();
    assert_eq!(updated, 1);

    // Now visible
    let visible = store.get_visible_resolution(&ref_id, "cl_res_1").unwrap();
    assert_eq!(visible.len(), 1);
    assert!(visible[0].is_visible);
    assert_eq!(visible[0].resolution_scope, "closure_reachable");
    assert_eq!(visible[0].semantic_confidence, "high");
    assert_eq!(visible[0].coverage_tier, "closure_complete");
    assert_eq!(visible[0].resolution_strategy, "closure_reachable");
    assert_eq!(visible[0].provenance.as_deref(), Some("import_based"));
}

// ── A2: Staged resolution is not visible ────────────────────────────────────

#[test]
fn test_staged_resolution_not_visible() {
    let store = test_store();
    let ref_id = make_ref_id("staged_only");

    store.insert_closure_generation("cl_staged").unwrap();

    store
        .insert_reference_resolution(
            &ref_id,
            "cl_staged",
            1,
            "project_wide",
            None,
            "partial",
            "medium",
            "project_wide",
            None,
        )
        .unwrap();

    // Visible query should return empty
    let visible = store.get_visible_resolution(&ref_id, "cl_staged").unwrap();
    assert!(
        visible.is_empty(),
        "staged resolution should not appear in visible queries"
    );
}

// ── A3: Batch make visible ──────────────────────────────────────────────────

#[test]
fn test_make_visible_batch() {
    let store = test_store();
    let ref_a = make_ref_id("fn_a");
    let ref_b = make_ref_id("fn_b");
    let ref_c = make_ref_id("fn_c");

    store.insert_closure_generation("cl_batch").unwrap();

    store
        .insert_reference_resolution(
            &ref_a,
            "cl_batch",
            1,
            "closure_reachable",
            None,
            "boundary",
            "certain",
            "closure_reachable",
            None,
        )
        .unwrap();
    store
        .insert_reference_resolution(
            &ref_b,
            "cl_batch",
            1,
            "closure_imports",
            None,
            "boundary",
            "certain",
            "closure_imports",
            None,
        )
        .unwrap();
    store
        .insert_reference_resolution(
            &ref_c,
            "cl_batch",
            1,
            "project_wide",
            None,
            "boundary",
            "certain",
            "project_wide",
            None,
        )
        .unwrap();

    // All 3 should be transitioned
    let updated = store.make_resolutions_visible("cl_batch", 1).unwrap();
    assert_eq!(updated, 3);

    assert_eq!(
        store
            .get_visible_resolution(&ref_a, "cl_batch")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .get_visible_resolution(&ref_b, "cl_batch")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .get_visible_resolution(&ref_c, "cl_batch")
            .unwrap()
            .len(),
        1
    );
}

// ── A4: Same reference in different closures ────────────────────────────────

#[test]
fn test_multiple_closures_same_reference() {
    let store = test_store();
    let ref_id = make_ref_id("shared_func");

    // Closure A
    store.insert_closure_generation("cl_a").unwrap();
    store
        .insert_reference_resolution(
            &ref_id,
            "cl_a",
            1,
            "closure_reachable",
            None,
            "closure_complete",
            "high",
            "closure_reachable",
            None,
        )
        .unwrap();
    store.make_resolutions_visible("cl_a", 1).unwrap();

    // Closure B — same reference_id, different closure
    store.insert_closure_generation("cl_b").unwrap();
    store
        .insert_reference_resolution(
            &ref_id,
            "cl_b",
            1,
            "closure_imports",
            None,
            "boundary",
            "medium",
            "closure_imports",
            None,
        )
        .unwrap();
    store.make_resolutions_visible("cl_b", 1).unwrap();

    // Each closure has its own resolution
    let a_resolutions = store.get_visible_resolution(&ref_id, "cl_a").unwrap();
    assert_eq!(a_resolutions.len(), 1);
    assert_eq!(a_resolutions[0].closure_id, "cl_a");
    assert_eq!(a_resolutions[0].resolution_strategy, "closure_reachable");

    let b_resolutions = store.get_visible_resolution(&ref_id, "cl_b").unwrap();
    assert_eq!(b_resolutions.len(), 1);
    assert_eq!(b_resolutions[0].closure_id, "cl_b");
    assert_eq!(b_resolutions[0].resolution_strategy, "closure_imports");

    // Different strategies for the same reference
    assert_ne!(
        a_resolutions[0].resolution_strategy,
        b_resolutions[0].resolution_strategy
    );
}

// ── A5: Resolution counts ───────────────────────────────────────────────────

#[test]
fn test_get_resolution_counts() {
    let store = test_store();
    let r1 = make_ref_id("count_a");
    let r2 = make_ref_id("count_b");
    let r3 = make_ref_id("count_c");

    store.insert_closure_generation("cl_counts").unwrap();

    // 2x closure_reachable, 1x closure_imports
    store
        .insert_reference_resolution(
            &r1,
            "cl_counts",
            1,
            "closure_reachable",
            None,
            "closure_complete",
            "high",
            "closure_reachable",
            None,
        )
        .unwrap();
    store
        .insert_reference_resolution(
            &r2,
            "cl_counts",
            1,
            "closure_reachable",
            None,
            "closure_complete",
            "high",
            "closure_reachable",
            None,
        )
        .unwrap();
    store
        .insert_reference_resolution(
            &r3,
            "cl_counts",
            1,
            "closure_imports",
            None,
            "boundary",
            "low",
            "closure_imports",
            None,
        )
        .unwrap();

    store.make_resolutions_visible("cl_counts", 1).unwrap();

    let counts = store.get_resolution_counts("cl_counts").unwrap();
    assert_eq!(counts.len(), 2, "expected 2 strategy groups");

    let total: i64 = counts.iter().map(|(_, c)| c).sum();
    assert_eq!(total, 3);

    // Find reachable count
    let reachable_count = counts
        .iter()
        .find(|(strat, _)| strat == "closure_reachable")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(reachable_count, 2);

    let imports_count = counts
        .iter()
        .find(|(strat, _)| strat == "closure_imports")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(imports_count, 1);
}

// ── A6: Different resolution strategies ─────────────────────────────────────

#[test]
fn test_resolution_different_strategies() {
    let store = test_store();
    let r1 = make_ref_id("strat_a");
    let r2 = make_ref_id("strat_b");
    let r3 = make_ref_id("strat_c");

    store.insert_closure_generation("cl_strategies").unwrap();

    store
        .insert_reference_resolution(
            &r1,
            "cl_strategies",
            1,
            "closure_reachable",
            None,
            "closure_complete",
            "high",
            "closure_reachable",
            None,
        )
        .unwrap();
    store
        .insert_reference_resolution(
            &r2,
            "cl_strategies",
            1,
            "closure_imports",
            None,
            "boundary",
            "medium",
            "closure_imports",
            None,
        )
        .unwrap();
    store
        .insert_reference_resolution(
            &r3,
            "cl_strategies",
            1,
            "project_wide",
            None,
            "partial",
            "low",
            "project_wide",
            None,
        )
        .unwrap();

    store.make_resolutions_visible("cl_strategies", 1).unwrap();

    let a = &store.get_visible_resolution(&r1, "cl_strategies").unwrap()[0];
    let b = &store.get_visible_resolution(&r2, "cl_strategies").unwrap()[0];
    let c = &store.get_visible_resolution(&r3, "cl_strategies").unwrap()[0];

    assert_eq!(a.resolution_strategy, "closure_reachable");
    assert_eq!(b.resolution_strategy, "closure_imports");
    assert_eq!(c.resolution_strategy, "project_wide");
}

// ── A7: Semantic confidence levels ──────────────────────────────────────────

#[test]
fn test_resolution_semantic_confidence_levels() {
    let store = test_store();
    let refs: Vec<Vec<u8>> = (0..4).map(|i| make_ref_id(&format!("conf_{i}"))).collect();

    store.insert_closure_generation("cl_confidence").unwrap();

    let levels = ["certain", "high", "medium", "low"];
    for (i, level) in levels.iter().enumerate() {
        store
            .insert_reference_resolution(
                &refs[i],
                "cl_confidence",
                1,
                "closure_reachable",
                None,
                "closure_complete",
                level,
                "closure_reachable",
                None,
            )
            .unwrap();
    }

    store.make_resolutions_visible("cl_confidence", 1).unwrap();

    for (i, expected_level) in levels.iter().enumerate() {
        let resolutions = store
            .get_visible_resolution(&refs[i], "cl_confidence")
            .unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].semantic_confidence, *expected_level,
            "expected semantic_confidence {expected_level} for ref {i}"
        );
    }
}

// ── A8: Coverage tier variants ──────────────────────────────────────────────

#[test]
fn test_resolution_coverage_tier_variants() {
    let store = test_store();
    let refs: Vec<Vec<u8>> = (0..4).map(|i| make_ref_id(&format!("tier_{i}"))).collect();

    store.insert_closure_generation("cl_tiers").unwrap();

    let tiers = ["closure_complete", "boundary", "partial", "manifest"];
    for (i, tier) in tiers.iter().enumerate() {
        store
            .insert_reference_resolution(
                &refs[i],
                "cl_tiers",
                1,
                "closure_reachable",
                None,
                tier,
                "high",
                "closure_reachable",
                None,
            )
            .unwrap();
    }

    store.make_resolutions_visible("cl_tiers", 1).unwrap();

    for (i, expected_tier) in tiers.iter().enumerate() {
        let resolutions = store.get_visible_resolution(&refs[i], "cl_tiers").unwrap();
        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].coverage_tier, *expected_tier,
            "expected coverage_tier {expected_tier} for ref {i}"
        );
    }
}
