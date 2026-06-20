//! Tests for closure_generations store module.

use super::*;

fn test_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    store
}

#[test]
fn test_insert_and_commit_closure() {
    let store = test_store();

    // Insert a closure
    store.insert_closure_generation("cl_test_1").unwrap();

    // Verify state is 'building'
    let cg = store.get_closure_generation("cl_test_1").unwrap().unwrap();
    assert_eq!(cg.state, "building");
    assert_eq!(cg.committed_generation, 0);

    // Commit the closure
    let generation = store.commit_closure_generation("cl_test_1").unwrap();

    // Verify state is 'committed' and generation > 0
    let cg = store.get_closure_generation("cl_test_1").unwrap().unwrap();
    assert_eq!(cg.state, "committed");
    assert_eq!(cg.committed_generation, generation);
    assert!(generation > 0);
}

#[test]
fn test_mark_stale() {
    let store = test_store();

    store.insert_closure_generation("cl_stale").unwrap();
    store.commit_closure_generation("cl_stale").unwrap();

    // Mark stale
    store.mark_closure_stale("cl_stale").unwrap();

    let cg = store.get_closure_generation("cl_stale").unwrap().unwrap();
    assert_eq!(cg.state, "stale");
}

#[test]
fn test_get_committed_generation_not_found() {
    let store = test_store();

    let generation = store.get_committed_generation("nonexistent").unwrap();
    assert!(generation.is_none());
}

#[test]
fn test_double_insert_idempotent() {
    let store = test_store();

    // First insert succeeds
    store.insert_closure_generation("cl_idem_1").unwrap();

    // Second insert with same closure_id should be ignored, not error
    store.insert_closure_generation("cl_idem_1").unwrap();

    // Verify still only one row in 'building' state
    let cg = store.get_closure_generation("cl_idem_1").unwrap().unwrap();
    assert_eq!(cg.state, "building");
    assert_eq!(cg.committed_generation, 0);
}

#[test]
fn test_double_commit_increments() {
    let store = test_store();

    store.insert_closure_generation("cl_double").unwrap();

    let gen1 = store.commit_closure_generation("cl_double").unwrap();
    assert_eq!(gen1, 1);

    let gen2 = store.commit_closure_generation("cl_double").unwrap();
    assert_eq!(gen2, 2);

    let cg = store.get_closure_generation("cl_double").unwrap().unwrap();
    assert_eq!(cg.committed_generation, 2);
}

#[test]
fn test_prune_committed_closures_removes_old_transient_facts() {
    let store = test_store();
    let file_id = types::FileId::generate("src/focus.c");
    let reference_id = vec![7_u8; 32];

    for index in 0..4 {
        let closure_id = format!("cl_{index}");
        store.insert_closure_generation(&closure_id).unwrap();
        store
            .insert_closure_coverage(&closure_id, file_id.as_bytes(), "seed_file", 0, None)
            .unwrap();
        store
            .insert_reference_resolution(
                &reference_id,
                &closure_id,
                0,
                "boundary",
                Some(&[index as u8; 32]),
                "boundary",
                "certain",
                "name_only",
                None,
            )
            .unwrap();
        store.commit_closure_generation(&closure_id).unwrap();
    }

    assert_eq!(store.prune_committed_closures(2).unwrap(), 2);
    assert!(store.get_closure_generation("cl_0").unwrap().is_none());
    assert!(store.get_closure_generation("cl_1").unwrap().is_none());
    assert!(store.get_closure_generation("cl_2").unwrap().is_some());
    assert!(store.get_closure_generation("cl_3").unwrap().is_some());
    assert_eq!(store.count_reference_resolutions("cl_0", 0).unwrap(), 0);
    assert!(store.get_coverage_counts("cl_0").unwrap().is_empty());
}

#[test]
fn test_reset_focus_session_state_clears_control_plane_facts() {
    let store = test_store();
    let closure_id = "cl_previous_session";
    let file_id = types::FileId::generate("src/focus.c");
    store.insert_closure_generation(closure_id).unwrap();
    store
        .insert_closure_coverage(closure_id, file_id.as_bytes(), "seed_file", 0, None)
        .unwrap();
    store
        .insert_reference_resolution(
            &[3_u8; 32],
            closure_id,
            0,
            "boundary",
            Some(&[4_u8; 32]),
            "boundary",
            "certain",
            "name_only",
            None,
        )
        .unwrap();
    store.commit_closure_generation(closure_id).unwrap();

    store.reset_focus_session_state().unwrap();

    assert!(store.get_closure_generation(closure_id).unwrap().is_none());
    assert!(store.get_coverage_counts(closure_id).unwrap().is_empty());
    assert_eq!(store.count_reference_resolutions(closure_id, 0).unwrap(), 0);
}
