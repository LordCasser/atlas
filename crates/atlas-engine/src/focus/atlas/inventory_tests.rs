//! Tests for the FileInventoryBuilder (Tier 0 bootstrap).
//!
//! These tests create temporary project directories with source files
//! and verify that the inventory builder correctly discovers, stats,
//! and inserts them into the file_inventory table.

use std::fs;
use std::sync::Arc;

use db::Store;

use super::inventory::FileInventoryBuilder;

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

#[test]
fn test_file_inventory_populate_discovers_files() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();

    // Write test files with recognized extensions
    write_file(root, "src/main.rs", "fn main() {}");
    write_file(root, "src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }");
    write_file(root, "src/utils.ts", "export function helper() {}");

    let store = test_store();
    let builder = FileInventoryBuilder::new(store.clone(), root.to_path_buf());

    let count = builder.populate().unwrap();
    assert!(count >= 3, "expected at least 3 files, got {count}");

    // Verify each file is queryable
    let row = store
        .find_file_inventory_by_path("src/main.rs")
        .unwrap()
        .expect("src/main.rs should exist");
    assert_eq!(row.language, "rust");
    assert_eq!(row.size, "fn main() {}".len() as i64);

    let row = store
        .find_file_inventory_by_path("src/lib.rs")
        .unwrap()
        .expect("src/lib.rs should exist");
    assert_eq!(row.language, "rust");

    let row = store
        .find_file_inventory_by_path("src/utils.ts")
        .unwrap()
        .expect("src/utils.ts should exist");
    assert_eq!(row.language, "typescript");

    // Verify count
    assert_eq!(store.file_inventory_count().unwrap(), count);
}

#[test]
fn test_file_inventory_mtime_and_size() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();

    let content = "fn hello() { return 42; }";
    write_file(root, "hello.rs", content);

    let store = test_store();
    let builder = FileInventoryBuilder::new(store.clone(), root.to_path_buf());
    builder.populate().unwrap();

    let row = store
        .find_file_inventory_by_path("hello.rs")
        .unwrap()
        .expect("hello.rs should exist");

    assert_eq!(row.size, content.len() as i64);
    assert!(row.mtime > 0, "mtime should be a positive timestamp");

    #[cfg(unix)]
    {
        assert!(row.inode > 0, "inode should be positive on unix");
        // dev may be 0 on some platforms, but typically > 0
    }
}

#[test]
fn test_file_inventory_empty_project() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();

    let store = test_store();
    let builder = FileInventoryBuilder::new(store.clone(), root.to_path_buf());

    // No source files → discover_files returns 0
    let count = builder.populate().unwrap();
    assert_eq!(count, 0);
    assert_eq!(store.file_inventory_count().unwrap(), 0);
}

#[test]
fn test_file_inventory_idempotent_populate() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();

    write_file(root, "main.rs", "fn main() {}");

    let store = test_store();
    let builder = FileInventoryBuilder::new(store.clone(), root.to_path_buf());

    // First populate
    let count1 = builder.populate().unwrap();
    assert_eq!(count1, 1);

    // Second populate (INSERT OR REPLACE — should not duplicate)
    let count2 = builder.populate().unwrap();
    assert_eq!(count2, 1);
    assert_eq!(store.file_inventory_count().unwrap(), 1);
}
