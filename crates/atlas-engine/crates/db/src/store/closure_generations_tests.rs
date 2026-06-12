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

    let generation = store
        .get_committed_generation("nonexistent")
        .unwrap();
    assert!(generation.is_none());
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
