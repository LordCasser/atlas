//! E2E tests for lazy index features: scope + manifest + lazy structural.
//!
//! Uses TypeScript (.ts) files which are in the default feature set.
//!
//! Run: `cargo test --test lazy_index_e2e`

use atlas_cli::commands::index;
use atlas_cli::runtime::{CommandContext, DbMode};
use atlas_engine::Store;
use atlas_engine::{layer, status};
use std::sync::Arc;
use tempfile::TempDir;

fn setup_project(files: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("create temp dir");
    for (rel_path, content) in files {
        let file_path = tmp.path().join(rel_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&file_path, content).expect("write source file");
    }
    tmp
}

fn open_store(tmp: &TempDir) -> Arc<Store> {
    Arc::new(Store::open_db(&tmp.path().join(".atlas/atlas.db")).expect("open store"))
}

// ───────────────────────────────────────────────────────────────────────────
// P0: Scope Index
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn p0_scope_index_filters_by_include() {
    let tmp = setup_project(&[
        ("src/main.ts", "function main(): void {}"),
        ("src/lib.ts", "export function add(): number { return 42; }"),
        ("tests/test.ts", "// test file"),
        ("benches/bench.ts", "// bench file"),
    ]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &["src/**".to_string()], &[], &[], "structural").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.contains("src/main.ts")));
    assert!(paths.iter().any(|p| p.contains("src/lib.ts")));
    assert!(!paths.iter().any(|p| p.contains("tests/")));
    assert!(!paths.iter().any(|p| p.contains("benches/")));
}

#[test]
fn p0_scope_index_records_metadata() {
    let tmp = setup_project(&[("src/index.ts", "const x = 1;")]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &["src/**".to_string()], &[], &[], "structural").expect("atlas index");

    let store = open_store(&tmp);
    let scope = store.get_metadata("indexed_scope").unwrap();
    assert!(scope.is_some());
    assert!(scope.unwrap().contains("src/**"));
}

#[test]
fn p0_scope_sugar_converts_dir_to_glob() {
    let tmp = setup_project(&[
        ("a/x.ts", "export function x(): void {}"),
        ("b/y.ts", "export function y(): void {}"),
        ("c/z.ts", "export function z(): void {}"),
    ]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    let scopes = vec!["a".to_string(), "c".to_string()];
    index::run(&project, &[], &scopes, &[], "structural").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.iter().any(|p| p.contains("a/x.ts")));
    assert!(paths.iter().any(|p| p.contains("c/z.ts")));
    assert!(!paths.iter().any(|p| p.contains("b/y.ts")));
}

// ───────────────────────────────────────────────────────────────────────────
// P1: Manifest Index
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn p1_manifest_produces_top_level_symbols() {
    // manifest.scm wraps patterns in (program ...) — only captures top-level
    let tmp = setup_project(&[(
        "src/lib.ts",
        "// top-level\n\
         export function topLevel(): number { return 42; }\n\
         export class MyClass {\n\
             // method — NOT top-level, should not appear in manifest\n\
             method(): string { return 'hello'; }\n\
             field: number;\n\
         }\n\
         export const TOP_CONST = 100;\n",
    )]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;
    let symbols = store.find_symbols_by_file(&fid).unwrap();

    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"topLevel"),
        "topLevel fn should be in manifest, got {:?}",
        names
    );
    assert!(
        names.contains(&"MyClass"),
        "MyClass should be in manifest, got {:?}",
        names
    );

    // All manifest symbols should have layer=manifest
    for sym in &symbols {
        assert_eq!(
            sym.layer,
            layer::MANIFEST,
            "symbol {} has wrong layer",
            sym.name
        );
    }
}

#[test]
fn p1_manifest_writes_file_extraction_state() {
    let tmp = setup_project(&[("lib.ts", "export function hello(): void {}")]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;
    let layer_rec = store
        .get_file_extraction_state(&fid, layer::MANIFEST)
        .unwrap();
    assert!(layer_rec.is_some(), "manifest layer should be recorded");
    let (s, _hash) = layer_rec.unwrap();
    assert_eq!(s, status::COMPLETE);
}

#[test]
fn p1_structural_writes_structural_layer() {
    let tmp = setup_project(&[("lib.ts", "export function hello(): void {}")]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "structural").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;
    let layer = store
        .get_file_extraction_state(&fid, layer::STRUCTURAL)
        .unwrap();
    assert!(layer.is_some());
    let (s, _) = layer.unwrap();
    assert_eq!(s, status::COMPLETE);
}

// ───────────────────────────────────────────────────────────────────────────
// P2: Lazy Structural
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn p2_lazy_detects_missing_structural_layer() {
    let tmp = setup_project(&[("lib.ts", "export function empty(): void {}")]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;

    let svc =
        atlas_engine::LazyStructuralService::new(store.clone(), Some(tmp.path().to_path_buf()));
    assert!(
        !svc.has_structural_layer(&fid).unwrap(),
        "no structural layer after manifest-only"
    );
}

#[test]
fn p2_lazy_builds_structural_on_demand() {
    // Class with methods: manifest captures only the class, structural captures
    // class + methods → structural should have more symbols.
    let tmp = setup_project(&[(
        "lib.ts",
        "export class Calculator {\n\
             add(a: number, b: number): number { return a + b; }\n\
             sub(a: number, b: number): number { return a - b; }\n\
         }\n\
         export function standalone(): void {}\n",
    )]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;

    let before = store.find_symbols_by_file(&fid).unwrap();
    let count_before = before.len();
    for s in &before {
        assert_eq!(s.layer, layer::MANIFEST);
    }

    let svc =
        atlas_engine::LazyStructuralService::new(store.clone(), Some(tmp.path().to_path_buf()));
    let result = svc.ensure_structural_for_file(&fid, None).unwrap();
    assert!(result.files_built >= 1);

    let after = store.find_symbols_by_file(&fid).unwrap();
    assert!(
        after.len() > count_before,
        "structural should have more symbols than manifest ({} > {})",
        after.len(),
        count_before
    );
    assert!(svc.has_structural_layer(&fid).unwrap());
}

#[test]
fn p2_lazy_cache_hit_skips_rebuild() {
    let tmp = setup_project(&[("lib.ts", "function a(): void {}\nfunction b(): void {}")]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;

    let svc =
        atlas_engine::LazyStructuralService::new(store.clone(), Some(tmp.path().to_path_buf()));
    let r1 = svc.ensure_structural_for_file(&fid, None).unwrap();
    assert!(r1.files_built >= 1);
    assert_eq!(r1.files_cached, 0);

    let r2 = svc.ensure_structural_for_file(&fid, None).unwrap();
    assert_eq!(r2.files_built, 0);
    assert!(r2.files_cached >= 1);
}

#[test]
fn p2_lazy_preserves_existing_structural() {
    let tmp = setup_project(&[("lib.ts", "function a(): void {}\nfunction b(): void {}")]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "structural").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;

    let svc =
        atlas_engine::LazyStructuralService::new(store.clone(), Some(tmp.path().to_path_buf()));
    let result = svc.ensure_structural_for_file(&fid, None).unwrap();
    assert_eq!(result.files_built, 0);
    assert!(result.files_cached >= 1);
}

#[test]
fn p2_lazy_ensure_for_symbol_by_name() {
    // Class with methods: manifest captures Calculator, structural adds methods
    let tmp = setup_project(&[(
        "lib.ts",
        "export class Calculator {\n\
             compute(x: number): number { return x * 2; }\n\
         }\n",
    )]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index");

    let store = open_store(&tmp);
    let svc =
        atlas_engine::LazyStructuralService::new(store.clone(), Some(tmp.path().to_path_buf()));

    let result = svc.ensure_structural_for_symbol("Calculator").unwrap();
    assert!(result.files_built >= 1);

    let files = store.list_files().unwrap();
    let fid = files[0].file_id;
    assert!(svc.has_structural_layer(&fid).unwrap());

    let symbols = store.find_symbols_by_file(&fid).unwrap();
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Calculator" && s.layer == layer::STRUCTURAL)
    );
    // compute is a method — only appears in structural
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "compute" && s.layer == layer::STRUCTURAL)
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn layer_constants_match_strings() {
    assert_eq!(layer::MANIFEST, "manifest");
    assert_eq!(layer::STRUCTURAL, "structural");
    assert_eq!(layer::DATAFLOW, "dataflow");
    assert_eq!(status::COMPLETE, "complete");
    assert_eq!(status::PARTIAL, "partial");
    assert_eq!(status::FAILED, "failed");
}
