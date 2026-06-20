//! Tests for BootstrapManager — lazy background bootstrap for focus-driven analysis.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use db::Store;
use types::FileId;

use super::bootstrap::BootstrapManager;

fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
}

/// Helper: poll predicate every 100ms until true or timeout_ms elapsed.
fn wait_for(predicate: impl Fn() -> bool, timeout_ms: u64) -> bool {
    let start = Instant::now();
    while !predicate() {
        if start.elapsed().as_millis() as u64 > timeout_ms {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
    true
}

// ── Test 1: new → not started ──────────────────────────────────────────────

#[test]
fn test_bootstrap_new_not_started() {
    let store = test_store();
    let mgr = BootstrapManager::new(store, None);
    assert!(!mgr.is_minimum_ready());
}

// ── Test 2: start → tier0 completes ────────────────────────────────────────

#[test]
fn test_bootstrap_start_tier0_completes() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    write_file(&root, "main.c", "int main() { return 0; }");
    write_file(&root, "lib.c", "int add(int a, int b) { return a + b; }");
    write_file(&root, "util.c", "void helper() {}");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    assert!(!mgr.is_minimum_ready());
    mgr.start();

    let ready = wait_for(|| mgr.is_minimum_ready(), 5000);
    assert!(ready, "tier0 did not complete within 5s");
}

// ── Test 3: start() idempotent ─────────────────────────────────────────────

#[test]
fn test_bootstrap_start_idempotent() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    write_file(&root, "main.c", "int main() { return 0; }");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    // First start — should spawn thread
    mgr.start();
    // Second start — should be no-op (does not panic)
    mgr.start();

    // Wait for completion
    let ready = wait_for(|| mgr.is_minimum_ready(), 5000);
    assert!(ready, "tier0 did not complete within 5s");
}

// ── Test 4: cancel stops thread ────────────────────────────────────────────

#[test]
fn test_bootstrap_cancel_stops_thread() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    // Create many files to ensure bootstrap takes long enough to cancel
    for i in 0..200 {
        write_file(&root, &format!("file_{i}.c"), "int x;");
    }

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    mgr.start();
    // Give it a moment to begin
    thread::sleep(Duration::from_millis(50));
    mgr.cancel();

    // Wait to verify the thread terminates
    thread::sleep(Duration::from_millis(500));

    // After cancel, the thread should stop. Since we cancelled quickly,
    // tier0 may or may not be complete — we just verify we don't hang.
    // The thread should be joinable (implicitly via Drop).
}

// ── Test 5: tier0 populates inventory ──────────────────────────────────────

#[test]
fn test_bootstrap_tier0_populates_inventory() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    write_file(&root, "main.c", "int main() { return 0; }");
    write_file(&root, "lib.c", "int add(int a, int b) { return a + b; }");
    write_file(&root, "src/util.c", "void helper() {}");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    mgr.start();
    let ready = wait_for(|| mgr.is_minimum_ready(), 5000);
    assert!(ready, "tier0 did not complete within 5s");

    // Verify file_inventory has all 3 files
    assert_eq!(store.file_inventory_count().unwrap(), 3);

    // Verify each file is queryable
    let row = store
        .find_file_inventory_by_path("main.c")
        .unwrap()
        .expect("main.c should exist");
    assert_eq!(row.language, "c");

    let row = store
        .find_file_inventory_by_path("src/util.c")
        .unwrap()
        .expect("src/util.c should exist");
    assert_eq!(row.language, "c");
}

// ── Test 6: tier0 noop when already done ───────────────────────────────────

#[test]
fn test_bootstrap_tier0_noop_when_already_done() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    write_file(&root, "main.c", "int main() { return 0; }");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    mgr.start();
    let ready = wait_for(|| mgr.is_minimum_ready(), 5000);
    assert!(ready, "first tier0 did not complete within 5s");

    // Calling start() again when already done should not error
    mgr.start();
    // Still complete
    assert!(mgr.is_minimum_ready());
    // Count should still be 1 (no duplicate)
    assert_eq!(store.file_inventory_count().unwrap(), 1);
}

#[test]
fn test_bootstrap_skips_project_wide_work_for_persistent_inventory() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    write_file(&root, "already_known.c", "void known(void) {}");
    write_file(&root, "must_not_be_discovered.c", "void hidden(void) {}");

    let store = test_store();
    let known_id = FileId::generate("already_known.c");
    store
        .insert_file_inventory(&known_id, "already_known.c", "c", 0, 20, 0, 0)
        .unwrap();
    let mut manager = BootstrapManager::new(store.clone(), Some(root));

    manager.start();

    assert!(manager.is_minimum_ready());
    assert!(manager.is_tier1_hot_complete());
    assert_eq!(store.file_inventory_count().unwrap(), 1);
    assert!(
        store
            .find_file_inventory_by_path("must_not_be_discovered.c")
            .unwrap()
            .is_none()
    );
}

// ── Test 7: ensure_minimum_ready blocks ────────────────────────────────────

#[test]
fn test_ensure_minimum_ready_blocks() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    write_file(&root, "main.c", "int main() { return 0; }");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    // Start bootstrap — it runs in background
    mgr.start();

    // ensure_minimum_ready should block until tier0 completes
    let start = Instant::now();
    mgr.ensure_minimum_ready();
    let elapsed = start.elapsed();

    // It must have returned
    assert!(mgr.is_minimum_ready());
    // Should have taken at least some time (it was blocking)
    assert!(
        elapsed.as_millis() > 0,
        "ensure_minimum_ready returned too fast"
    );

    // Calling again should return immediately
    let start2 = Instant::now();
    mgr.ensure_minimum_ready();
    assert!(
        start2.elapsed().as_millis() < 500,
        "ensure_minimum_ready should return immediately when already ready"
    );
}

// ── Test 8: tier1 populates hints ──────────────────────────────────────────

#[test]
fn test_bootstrap_tier1_populates_hints() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    // Write a Rust file with a simple function — manifest extraction
    // should find at least "hello" as a top-level symbol.
    write_file(&root, "hello.rs", "pub fn hello() -> i32 { 42 }");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    mgr.start();

    // Wait for tier0 first
    mgr.ensure_minimum_ready();

    // Poll for symbol hints to appear (tier1 completion)
    let hints_found = wait_for(|| store.has_symbol_hints("hello").unwrap_or(false), 10000);
    assert!(
        hints_found,
        "symbol hints did not appear for 'hello' within 10s"
    );

    // Verify the hint is correct
    let hints = store.query_symbol_hints("hello").unwrap();
    assert!(!hints.is_empty(), "expected at least one hint for 'hello'");
    let hint = &hints[0];
    assert_eq!(hint.name, "hello");
    assert_eq!(hint.kind, "function");
    assert_eq!(hint.source, "manifest");
    assert!(hint.confidence > 0.0);
}

// ── Test 9: empty project ──────────────────────────────────────────────────

#[test]
fn test_bootstrap_empty_project() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(root));

    mgr.start();

    let ready = wait_for(|| mgr.is_minimum_ready(), 5000);
    assert!(ready, "tier0 did not complete within 5s (empty project)");

    // No files discovered
    assert_eq!(store.file_inventory_count().unwrap(), 0);
}

// ── Test 10: non-existent project ──────────────────────────────────────────

#[test]
fn test_bootstrap_non_existent_project() {
    let non_existent = PathBuf::from("/tmp/atlas_test_nonexistent_dir_12345");

    let store = test_store();
    let mut mgr = BootstrapManager::new(store.clone(), Some(non_existent));

    mgr.start();

    let ready = wait_for(|| mgr.is_minimum_ready(), 5000);
    assert!(
        ready,
        "tier0 did not complete within 5s (non-existent project)"
    );

    // discover_files should return 0 for non-existent directory
    assert_eq!(store.file_inventory_count().unwrap(), 0);
}

// ── Test 11: tier2 extracts manifest ────────────────────────────────────────

#[test]
fn test_bootstrap_tier2_extracts_manifest() {
    use std::sync::atomic::AtomicBool;

    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    // Write a Rust file with top-level functions
    write_file(
        &root,
        "hello.rs",
        "pub fn hello() -> i32 { 42 }\npub fn world() -> &'static str { \"earth\" }",
    );

    let store = test_store();

    // Set up file_inventory with fingerprint (simulate Tier 0 + 0.5 done)
    let rel_path = "hello.rs";
    let abs_path = root.join(rel_path);
    let file_id = types::ids::FileId::generate(rel_path);
    let metadata = fs::metadata(&abs_path).unwrap();
    let mtime = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let language = types::Language::from_path(std::path::Path::new(rel_path)).unwrap();

    store
        .insert_file_inventory(
            &file_id,
            rel_path,
            language.as_str(),
            mtime,
            metadata.len() as i64,
            0,
            0,
        )
        .unwrap();

    let content = fs::read(&abs_path).unwrap();
    let hash = blake3::hash(&content).to_hex().to_string();
    store.set_file_fingerprint(&file_id, &hash).unwrap();

    // Run tier2
    let running = AtomicBool::new(true);
    let count = super::bootstrap::bootstrap_tier2(&store, &root, &running).unwrap();

    assert_eq!(count, 1, "tier2 should extract exactly 1 file");

    // Verify symbols table has entries
    let symbols = store.find_symbols_by_file(&file_id).unwrap();
    assert!(
        !symbols.is_empty(),
        "symbols table should have entries after tier2"
    );
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"hello"),
        "should contain 'hello': {names:?}"
    );
    assert!(
        names.contains(&"world"),
        "should contain 'world': {names:?}"
    );

    // Verify extraction_state records manifest as complete
    let state = store
        .get_file_extraction_state(&file_id, "manifest")
        .unwrap();
    assert!(
        state.is_some(),
        "extraction_state should have manifest record"
    );
    let (status, recorded_hash) = state.unwrap();
    assert_eq!(status, "complete");
    assert_eq!(recorded_hash, hash);

    let file = store.get_file(&file_id).unwrap().expect("file row");
    assert_eq!(
        file.path, rel_path,
        "tier2 must persist project-relative file paths so later full indexing can clean stale paths"
    );
}

// ── Test 12: tier2 skips already extracted files ────────────────────────────

#[test]
fn test_bootstrap_tier2_skips_already_extracted() {
    use std::sync::atomic::AtomicBool;

    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    write_file(&root, "one.rs", "pub fn one() -> i32 { 1 }");

    let store = test_store();

    let rel_path = "one.rs";
    let abs_path = root.join(rel_path);
    let file_id = types::ids::FileId::generate(rel_path);
    let metadata = fs::metadata(&abs_path).unwrap();
    let mtime = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let language = types::Language::from_path(std::path::Path::new(rel_path)).unwrap();

    store
        .insert_file_inventory(
            &file_id,
            rel_path,
            language.as_str(),
            mtime,
            metadata.len() as i64,
            0,
            0,
        )
        .unwrap();

    let content = fs::read(&abs_path).unwrap();
    let hash = blake3::hash(&content).to_hex().to_string();
    store.set_file_fingerprint(&file_id, &hash).unwrap();

    // First run — should extract 1 file
    let running = AtomicBool::new(true);
    let count1 = super::bootstrap::bootstrap_tier2(&store, &root, &running).unwrap();
    assert_eq!(count1, 1, "first run should extract 1 file");

    // Second run — should extract 0 (already done)
    let running = AtomicBool::new(true);
    let count2 = super::bootstrap::bootstrap_tier2(&store, &root, &running).unwrap();
    assert_eq!(
        count2, 0,
        "second run should extract 0 files (already complete)"
    );
}

// ── Test 13: tier2 respects cancellation ────────────────────────────────────

#[test]
fn test_bootstrap_tier2_respects_cancellation() {
    use std::sync::atomic::AtomicBool;

    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().to_path_buf();

    // Create several files
    for i in 0..20 {
        write_file(
            &root,
            &format!("file_{i}.rs"),
            &format!("pub fn f{i}() -> i32 {{ {i} }}"),
        );
    }

    let store = test_store();

    for i in 0..20 {
        let rel_path = format!("file_{i}.rs");
        let abs_path = root.join(&rel_path);
        let file_id = types::ids::FileId::generate(&rel_path);
        let metadata = fs::metadata(&abs_path).unwrap();
        let mtime = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let language = types::Language::from_path(std::path::Path::new(&rel_path)).unwrap();

        store
            .insert_file_inventory(
                &file_id,
                &rel_path,
                language.as_str(),
                mtime,
                metadata.len() as i64,
                0,
                0,
            )
            .unwrap();

        let content = fs::read(&abs_path).unwrap();
        let hash = blake3::hash(&content).to_hex().to_string();
        store.set_file_fingerprint(&file_id, &hash).unwrap();
    }

    // Run with cancellation already set to false — should extract 0
    let running = AtomicBool::new(false);
    let count = super::bootstrap::bootstrap_tier2(&store, &root, &running).unwrap();
    assert_eq!(count, 0, "cancelled before start should extract 0 files");

    // Run with cancellation set to true — should extract at least some files
    // (but we don't assert a specific count since batch size may vary)
    let running = AtomicBool::new(true);
    let count = super::bootstrap::bootstrap_tier2(&store, &root, &running).unwrap();
    assert!(
        count > 0,
        "with cancellation = true, should extract at least 1 file"
    );
}
