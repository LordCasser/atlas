//! E2E tests for lazy index features: scope + manifest + lazy structural.
//!
//! Uses TypeScript (.ts) files which are in the default feature set.
//!
//! Run: `cargo test --test lazy_index_e2e`

use atlas_cli::commands::index;
use atlas_cli::runtime::{CommandContext, DbMode};
use atlas_engine::Store;
use atlas_engine::enums::DataNodeKind;
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
        "topLevel fn should be in manifest, got {names:?}"
    );
    assert!(
        names.contains(&"MyClass"),
        "MyClass should be in manifest, got {names:?}"
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
// P3: Lazy Dataflow Capability Mask — CFG Gate (P1#4 regression test)
// ───────────────────────────────────────────────────────────────────────────

/// Full DB round-trip: structural index → lazy dataflow → read
/// `unit_extraction_state.capability_mask` and assert the CFG bit
/// follows the language profile + actual-CFG-nodes gate (loader.rs lines
/// 240-242).
///
/// Regression: if the gate is removed, PHP records would erroneously
/// carry the CFG bit and this test will fail.
#[test]
#[cfg(all(feature = "typescript", feature = "php"))]
fn p3_capability_mask_cfg_gated_by_language() {
    use atlas_engine::CapabilityMask;
    use atlas_engine::LazyDataflowService;

    // TypeScript function with if/else — should produce CFG nodes.
    // PHP function — cfg is FeatureSupport::unsupported in the profile.
    let tmp = setup_project(&[
        (
            "process.ts",
            "function process(x: number): number {\n\
                 if (x > 0) {\n\
                     return x * 2;\n\
                 }\n\
                 return 0;\n\
             }\n",
        ),
        (
            "greet.php",
            "<?php\n\
             function greet($name) {\n\
                 return \"Hello, \" . $name;\n\
             }\n",
        ),
    ]);
    let project = tmp.path().to_string_lossy().to_string();

    // 1. Run manifest + structural index (produces function symbols)
    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "structural").expect("atlas index");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let ts_file = files
        .iter()
        .find(|f| f.path.ends_with(".ts"))
        .expect("TS file");
    let php_file = files
        .iter()
        .find(|f| f.path.ends_with(".php"))
        .expect("PHP file");

    let svc = LazyDataflowService::new(store.clone(), Some(tmp.path().to_path_buf()));

    // ── TypeScript: trigger lazy dataflow ─────────────────────────────
    let ts_symbols = store.find_symbols_by_file(&ts_file.file_id).unwrap();
    let ts_fn = ts_symbols
        .iter()
        .find(|s| s.name == "process")
        .expect("TS function 'process' not found in symbols");
    let ts_window = svc
        .ensure_for_function(&ts_fn.id, None)
        .expect("TS lazy dataflow");
    assert!(
        ts_window.units_built >= 1,
        "TS dataflow should build at least 1 unit"
    );

    // Compute TS unit_id (first 16 bytes of symbol_id bytes)
    let mut ts_unit_id = [0u8; 16];
    ts_unit_id.copy_from_slice(&ts_fn.id.as_bytes()[..16]);
    let ts_state = store
        .get_unit_extraction_state(&ts_file.file_id, &ts_unit_id, layer::DATAFLOW)
        .unwrap()
        .expect("TS unit extraction state should exist after lazy dataflow");
    let ts_mask = ts_state.capability_mask;
    assert!(
        ts_mask.has(CapabilityMask::DATAFLOW),
        "TS must have DATAFLOW bit"
    );
    assert!(
        ts_mask.has(CapabilityMask::MANIFEST),
        "TS must have MANIFEST bit"
    );
    assert!(
        ts_mask.has(CapabilityMask::STRUCTURAL),
        "TS must have STRUCTURAL bit"
    );
    assert!(
        ts_mask.has(CapabilityMask::CALL_EDGES),
        "TS must have CALL_EDGES bit"
    );
    assert!(
        ts_mask.has(CapabilityMask::CFG),
        "TS function with control flow must have CFG bit set"
    );

    // ── PHP: trigger lazy dataflow ────────────────────────────────────
    let php_symbols = store.find_symbols_by_file(&php_file.file_id).unwrap();
    let php_fn = php_symbols
        .iter()
        .find(|s| s.name == "greet")
        .expect("PHP function 'greet' not found in symbols");
    let php_window = svc
        .ensure_for_function(&php_fn.id, None)
        .expect("PHP lazy dataflow");
    assert!(
        php_window.units_built >= 1,
        "PHP dataflow should build at least 1 unit"
    );

    // Compute PHP unit_id
    let mut php_unit_id = [0u8; 16];
    php_unit_id.copy_from_slice(&php_fn.id.as_bytes()[..16]);
    let php_state = store
        .get_unit_extraction_state(&php_file.file_id, &php_unit_id, layer::DATAFLOW)
        .unwrap()
        .expect("PHP unit extraction state should exist after lazy dataflow");
    let php_mask = php_state.capability_mask;
    assert!(
        php_mask.has(CapabilityMask::DATAFLOW),
        "PHP must have DATAFLOW bit"
    );
    assert!(
        php_mask.has(CapabilityMask::MANIFEST),
        "PHP must have MANIFEST bit"
    );
    assert!(
        php_mask.has(CapabilityMask::STRUCTURAL),
        "PHP must have STRUCTURAL bit"
    );
    assert!(
        php_mask.has(CapabilityMask::CALL_EDGES),
        "PHP must have CALL_EDGES bit"
    );
    assert!(
        !php_mask.has(CapabilityMask::CFG),
        "PHP must NOT have CFG bit — language profile declares cfg as unsupported"
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

// ───────────────────────────────────────────────────────────────────────────
// P2: Lazy Dataflow — Callsite ID remap (P0#1)
// ───────────────────────────────────────────────────────────────────────────

/// Verify that CallArg DataNodes produced by lazy dataflow extraction carry
/// real structural CallsiteIds, not provisional byte-offset values.
///
/// **Background**: LazyDataflow mode skips callsite extraction (mode.rs:27),
/// so CallArg DataNodes initially receive provisional `CallsiteId::from_file_byte`
/// IDs.  The loader (`loader.rs:160-210`) remaps them to real `CallsiteId::generate`
/// IDs by querying structural callsites already in the DB.
///
/// **Regression**: Without the cs_id_map remap, this test FAILS because
/// provisional byte-offset IDs never match real structural CallsiteIds.
#[test]
fn p2_lazy_dataflow_callsite_id_remap() {
    // ── Setup: TypeScript with a call chain that produces CallArg DataNodes ──
    // multiply(a, b) returns a*b; caller(x) calls multiply(x, 2) with two args.
    let tmp = setup_project(&[(
        "lib.ts",
        "function multiply(a: number, b: number): number {\n\
         \x20   return a * b;\n\
         }\n\
         \n\
         function caller(x: number): number {\n\
         \x20   const result = multiply(x, 2);\n\
         \x20   return result;\n\
         }\n",
    )]);
    let project = tmp.path().to_string_lossy().to_string();

    // Step 1: Structural index → produces real Callsites (with real CallsiteIds)
    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "structural").expect("atlas index structural");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files[0].file_id;

    // Precondition: structural callsites must exist (e.g. multiply(x, 2))
    let cs_structural: Vec<_> = store.find_callsites_by_file(&fid).unwrap();
    assert!(
        !cs_structural.is_empty(),
        "structural index must produce at least one callsite"
    );
    let structural_cs_ids: std::collections::HashSet<_> =
        cs_structural.iter().map(|cs| cs.id).collect();

    // No dataflow before lazy
    assert!(
        store.find_data_nodes_by_file(&fid).unwrap().is_empty(),
        "no data nodes before lazy dataflow"
    );

    // Step 2: Trigger lazy dataflow for the 'caller' function
    let symbols = store.find_symbols_by_name("caller").unwrap();
    assert!(
        !symbols.is_empty(),
        "'caller' symbol not found after structural index"
    );
    let caller_sym_id = symbols[0].id;

    let svc = atlas_engine::LazyDataflowService::new(store.clone(), Some(tmp.path().to_path_buf()));
    let _window = svc
        .ensure_for_function(&caller_sym_id, None)
        .expect("lazy dataflow ensure_for_function");

    // Step 3: Verify data nodes exist after lazy extraction
    let dn_after = store.find_data_nodes_by_file(&fid).unwrap();
    assert!(
        !dn_after.is_empty(),
        "data nodes must exist after lazy dataflow"
    );

    // Step 4: THE KEY ASSERTION — find CallArg DataNodes and verify their
    // callsite_ids match real structural CallsiteIds (not provisional
    // byte-offset values from extraction).
    //
    // Before the P0#1 fix: CallArg DataNodes would carry provisional
    // CallsiteId::from_file_byte(file_id, byte_offset) values, which
    // NEVER match the structural CallsiteId::generate(ref_id, caller,
    // byte_offset) values stored in the callsites table.
    let call_arg_nodes: Vec<_> = dn_after
        .iter()
        .filter(|dn| dn.kind == DataNodeKind::CallArg)
        .collect();
    assert!(
        !call_arg_nodes.is_empty(),
        "must have at least one CallArg DataNode (from multiply(x, 2) call)"
    );

    for dn in &call_arg_nodes {
        assert!(
            dn.callsite_id.is_some(),
            "CallArg DataNode must have a callsite_id after lazy dataflow"
        );
        let cs_id = dn.callsite_id.unwrap();
        assert!(
            structural_cs_ids.contains(&cs_id),
            "CallArg DataNode callsite_id {cs_id:?} must match a real structural CallsiteId.\n\
             Structural callsite IDs: {structural_cs_ids:?}\n\
             Without the cs_id_map remap (loader.rs L160-210), CallArg DataNodes carry\n\
             provisional byte-offset CallsiteIds that never match structural IDs."
        );
    }
}
