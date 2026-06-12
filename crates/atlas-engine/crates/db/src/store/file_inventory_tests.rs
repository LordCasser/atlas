//! Tests for file_inventory store module.

use super::*;
use types::ids::FileId;

fn test_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    store
}

fn make_file_id(name: &str) -> FileId {
    FileId::generate(name)
}

#[test]
fn test_file_inventory_insert_and_query() {
    let store = test_store();
    let file_id = make_file_id("src/main.rs");
    let path = "src/main.rs";

    store
        .insert_file_inventory(&file_id, path, "rust", 1_700_000_000, 1024, 12345, 42)
        .unwrap();

    let row = store
        .find_file_inventory_by_path(path)
        .unwrap()
        .expect("file should exist");
    assert_eq!(row.path, path);
    assert_eq!(row.language, "rust");
    assert_eq!(row.size, 1024);
    assert_eq!(row.mtime, 1_700_000_000);
    assert_eq!(row.inode, 12345);
    assert_eq!(row.dev, 42);
    assert!(row.content_hash.is_none());
}

#[test]
fn test_file_inventory_count() {
    let store = test_store();

    store
        .insert_file_inventory(&make_file_id("a.ts"), "a.ts", "typescript", 1, 100, 1, 1)
        .unwrap();
    store
        .insert_file_inventory(&make_file_id("b.py"), "b.py", "python", 2, 200, 2, 1)
        .unwrap();
    store
        .insert_file_inventory(&make_file_id("c.go"), "c.go", "go", 3, 300, 3, 1)
        .unwrap();

    assert_eq!(store.file_inventory_count().unwrap(), 3);
}

#[test]
fn test_file_inventory_find_by_path_not_found() {
    let store = test_store();

    let result = store.find_file_inventory_by_path("nonexistent.rs").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_set_fingerprint() {
    let store = test_store();
    let file_id = make_file_id("src/main.rs");
    let path = "src/main.rs";

    store
        .insert_file_inventory(&file_id, path, "rust", 1_700_000_000, 1024, 1, 1)
        .unwrap();

    store
        .set_file_fingerprint(&file_id, "abc123def456")
        .unwrap();

    let row = store
        .find_file_inventory_by_path(path)
        .unwrap()
        .expect("file should exist");
    assert_eq!(row.content_hash.as_deref(), Some("abc123def456"));
}

#[test]
fn test_get_unfingerprinted() {
    let store = test_store();

    // Insert 2 files with fingerprint, 1 without
    let f1 = make_file_id("hashed1.ts");
    let f2 = make_file_id("hashed2.ts");
    let f3 = make_file_id("unhashed.ts");

    store
        .insert_file_inventory(&f1, "hashed1.ts", "typescript", 1, 100, 1, 1)
        .unwrap();
    store
        .insert_file_inventory(&f2, "hashed2.ts", "typescript", 2, 200, 2, 1)
        .unwrap();
    store
        .insert_file_inventory(&f3, "unhashed.ts", "typescript", 3, 300, 3, 1)
        .unwrap();

    store.set_file_fingerprint(&f1, "aaa").unwrap();
    store.set_file_fingerprint(&f2, "bbb").unwrap();

    let unfingerprinted = store.get_unfingerprinted_files(10).unwrap();
    assert_eq!(unfingerprinted.len(), 1);
    assert_eq!(unfingerprinted[0].1, "unhashed.ts");
}

#[test]
fn test_file_inventory_replace() {
    let store = test_store();
    let file_id = make_file_id("src/lib.rs");
    let path = "src/lib.rs";

    // Insert initial
    store
        .insert_file_inventory(&file_id, path, "rust", 100, 500, 1, 1)
        .unwrap();

    // Replace (INSERT OR REPLACE)
    store
        .insert_file_inventory(&file_id, path, "rust", 200, 999, 2, 2)
        .unwrap();

    let row = store
        .find_file_inventory_by_path(path)
        .unwrap()
        .expect("file should exist");
    assert_eq!(row.size, 999);
    assert_eq!(row.mtime, 200);
}

#[test]
fn test_find_file_inventory_path() {
    let store = test_store();
    let file_id = make_file_id("src/utils.rs");

    store
        .insert_file_inventory(&file_id, "src/utils.rs", "rust", 1, 100, 1, 1)
        .unwrap();

    let path = store
        .find_file_inventory_path(&file_id)
        .unwrap()
        .expect("path should be found");
    assert_eq!(path, "src/utils.rs");
}

#[test]
fn test_find_file_inventory_path_not_found() {
    let store = test_store();
    let unknown = make_file_id("nonexistent.rs");

    let result = store.find_file_inventory_path(&unknown).unwrap();
    assert!(result.is_none());
}
