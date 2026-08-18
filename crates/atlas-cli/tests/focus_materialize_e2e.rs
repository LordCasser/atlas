//! E2E tests for Focus materialize + Index path: scope, manifest, on-demand structural,
//! and N5 neighborhood parity (Focus file/unit slices ≈ Index for the same scope).
//!
//! Uses TypeScript (.ts) files which are in the default feature set.
//! Exercises shipped CLI index + `LazyStructuralService` / Focus materialize APIs
//! (not a separate “lazy product” line).
//!
//! Run: `cargo test -p atlas-cli --test focus_materialize_e2e`

use atlas_cli::commands::index;
use atlas_cli::runtime::{CommandContext, DbMode};
use atlas_engine::enums::{DataFlowKind, DataNodeKind};
use atlas_engine::{
    AccessStrategy, FocusMaterialize, FocusRuntime, QueryIntent, Store, layer, status,
};
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
    let scope = serde_json::from_str::<serde_json::Value>(
        &store.get_metadata("indexed_scope").unwrap().unwrap(),
    )
    .unwrap();
    assert_eq!(
        scope,
        serde_json::json!({ "include": ["src/**"], "exclude": [] })
    );
    assert_eq!(
        store.get_metadata("indexed_pipeline_grade").unwrap(),
        Some("structural".into())
    );
}

#[test]
fn p0_exclude_index_records_scoped_metadata() {
    let tmp = setup_project(&[
        ("src/index.ts", "const x = 1;"),
        ("vendor/dep.ts", "const dep = 1;"),
    ]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &["vendor/**".to_string()], "full").expect("atlas index");

    let store = open_store(&tmp);
    let scope = serde_json::from_str::<serde_json::Value>(
        &store.get_metadata("indexed_scope").unwrap().unwrap(),
    )
    .unwrap();
    assert_eq!(
        scope,
        serde_json::json!({ "include": [], "exclude": ["vendor/**"] })
    );
    assert_eq!(
        store.get_metadata("indexed_pipeline_grade").unwrap(),
        Some("full".into())
    );
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
    use atlas_engine::FactCoverage;

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

    let svc = {
        let m = FocusMaterialize::open(store.clone(), Some(tmp.path().to_path_buf()));
        m.dataflow().clone()
    };

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
        ts_mask.has(FactCoverage::DATAFLOW),
        "TS must have DATAFLOW bit"
    );
    assert!(
        ts_mask.has(FactCoverage::MANIFEST),
        "TS must have MANIFEST bit"
    );
    assert!(
        ts_mask.has(FactCoverage::STRUCTURAL),
        "TS must have STRUCTURAL bit"
    );
    assert!(
        !ts_mask.has(FactCoverage::CALL_EDGES),
        "TS function has no callsite, so CALL_EDGES must remain unset"
    );
    assert!(
        ts_mask.has(FactCoverage::CFG),
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
        php_mask.has(FactCoverage::DATAFLOW),
        "PHP must have DATAFLOW bit"
    );
    assert!(
        php_mask.has(FactCoverage::MANIFEST),
        "PHP must have MANIFEST bit"
    );
    assert!(
        php_mask.has(FactCoverage::STRUCTURAL),
        "PHP must have STRUCTURAL bit"
    );
    assert!(
        !php_mask.has(FactCoverage::CALL_EDGES),
        "PHP function has no callsite, so CALL_EDGES must remain unset"
    );
    assert!(
        php_mask.has(FactCoverage::CFG),
        "PHP lazy dataflow must persist CFG facts for the callable unit"
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

    let svc = {
        let m = FocusMaterialize::open(store.clone(), Some(tmp.path().to_path_buf()));
        m.dataflow().clone()
    };
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

// ───────────────────────────────────────────────────────────────────────────
// Post-extract hooks: index pipeline vs lazy structural (shared extract path)
// ───────────────────────────────────────────────────────────────────────────

const KERNEL_MODULE_SRC: &str = r#"
#include <linux/module.h>
#include <linux/init.h>

static int __init demo_init(void) { return 0; }
static void __exit demo_exit(void) {}

module_init(demo_init);
EXPORT_SYMBOL(demo_init);
EXPORT_SYMBOL_GPL(demo_exit);
"#;

/// CLI index structural path must persist EXPORT_SYMBOL + module_init edges
/// via the shared post-extract hook inside extract_file_with_mode.
#[test]
fn post_extract_index_structural_persists_export_and_initcall() {
    let tmp = setup_project(&[("drivers/demo.c", KERNEL_MODULE_SRC)]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "structural").expect("atlas index structural");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let file = files
        .iter()
        .find(|f| f.path.ends_with("demo.c"))
        .expect("demo.c indexed");

    let symbols = store.find_symbols_by_file(&file.file_id).unwrap();
    let init = symbols
        .iter()
        .find(|s| s.name == "demo_init")
        .expect("demo_init");
    let exit = symbols
        .iter()
        .find(|s| s.name == "demo_exit")
        .expect("demo_exit");
    assert!(init.exported, "index path must persist EXPORT_SYMBOL");
    assert!(exit.exported, "index path must persist EXPORT_SYMBOL_GPL");

    let edges = store.get_all_edges().unwrap();
    let initcall = edges
        .iter()
        .filter(|e| e.kind == atlas_engine::EdgeKind::RegistersCallback)
        .count();
    assert_eq!(
        initcall, 1,
        "index structural must persist module_init RegistersCallback edge"
    );
}

/// Lazy structural path (after manifest-only index) must apply the same hook
/// so EXPORT_SYMBOL / initcall do not diverge from full index.
#[test]
fn post_extract_lazy_structural_matches_index_export_semantics() {
    let tmp = setup_project(&[("drivers/demo.c", KERNEL_MODULE_SRC)]);
    let project = tmp.path().to_string_lossy().to_string();

    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "manifest").expect("atlas index manifest");

    let store = open_store(&tmp);
    let files = store.list_files().unwrap();
    let fid = files
        .iter()
        .find(|f| f.path.ends_with("demo.c"))
        .expect("demo.c")
        .file_id;

    // Manifest may already mark export if top-level; force structural via lazy.
    let svc =
        atlas_engine::LazyStructuralService::new(store.clone(), Some(tmp.path().to_path_buf()));
    let result = svc.ensure_structural_for_file(&fid, None).unwrap();
    assert!(result.files_built >= 1 || result.files_cached >= 1);

    let symbols = store.find_symbols_by_file(&fid).unwrap();
    let init = symbols
        .iter()
        .find(|s| s.name == "demo_init")
        .expect("demo_init after lazy structural");
    assert!(
        init.exported,
        "lazy structural must run shared post-extract EXPORT_SYMBOL hook"
    );

    let edges = store.get_all_edges().unwrap();
    let initcall = edges
        .iter()
        .filter(|e| e.kind == atlas_engine::EdgeKind::RegistersCallback)
        .count();
    assert_eq!(
        initcall, 1,
        "lazy structural must persist module_init edge like index path"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// N5: Focus neighborhood facts ≈ Index for the same files/units
//
// Product claim (architecture): after Focus materialize, closed-neighborhood
// experience ≈ those files had been Index'd. Not whole-DB equality.
// ───────────────────────────────────────────────────────────────────────────

/// Multi-file TS fixture:
/// - `seed.ts` calls into `math.ts` (should be structural-comparable)
/// - `peer.ts` is unrelated (must stay non-structural on Focus path)
const N5_NEIGHBORHOOD: &[(&str, &str)] = &[
    (
        "seed.ts",
        "import { add } from './math';\n\
         \n\
         export function useAdd(x: number): number {\n\
         \x20   const y = add(x, 1);\n\
         \x20   return y;\n\
         }\n",
    ),
    (
        "math.ts",
        "export function add(a: number, b: number): number {\n\
         \x20   return a + b;\n\
         }\n",
    ),
    (
        "peer.ts",
        "export function unrelated(): number {\n\
         \x20   return 99;\n\
         }\n",
    ),
];

fn file_by_suffix(store: &Store, suffix: &str) -> atlas_engine::FileId {
    store
        .list_files()
        .unwrap()
        .into_iter()
        .find(|f| f.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("missing file ending with {suffix}"))
        .file_id
}

/// Stable structural slice for one file (no job/runtime fields).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStructuralSlice {
    symbols: Vec<(String, String, u32, u32, bool)>,
    references: Vec<(String, String, u32, u32)>,
    callsites: Vec<(String, u32, u32, Option<String>)>,
    /// Edges with both endpoints in `file_symbol_ids` (intra-neighborhood).
    edges: Vec<(String, String, String)>,
}

fn structural_slice(store: &Store, file_id: &atlas_engine::FileId) -> FileStructuralSlice {
    let symbols = store.find_symbols_by_file(file_id).unwrap();
    let file_symbol_ids: std::collections::HashSet<_> = symbols.iter().map(|s| s.id).collect();

    let mut sym_keys: Vec<_> = symbols
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.kind.as_str().to_string(),
                s.range.start_byte,
                s.range.end_byte,
                s.exported,
            )
        })
        .collect();
    sym_keys.sort();

    let mut ref_keys: Vec<_> = store
        .find_references_by_file(file_id)
        .unwrap()
        .into_iter()
        .map(|r| {
            (
                r.name.clone(),
                r.kind.as_str().to_string(),
                r.range.start_byte,
                r.range.end_byte,
            )
        })
        .collect();
    ref_keys.sort();

    // Map symbol id → name for stable callsite/edge keys within this DB.
    let id_to_name: std::collections::HashMap<_, _> = store
        .list_files()
        .unwrap()
        .iter()
        .flat_map(|f| store.find_symbols_by_file(&f.file_id).unwrap())
        .map(|s| (s.id, s.name.clone()))
        .collect();

    let mut cs_keys: Vec<_> = store
        .find_callsites_by_file(file_id)
        .unwrap()
        .into_iter()
        .map(|cs| {
            (
                id_to_name
                    .get(&cs.caller)
                    .cloned()
                    .unwrap_or_else(|| format!("{:?}", cs.caller)),
                cs.range.start_byte,
                cs.range.end_byte,
                cs.receiver.clone(),
            )
        })
        .collect();
    cs_keys.sort();

    let mut edge_keys: Vec<_> = store
        .get_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| file_symbol_ids.contains(&e.source) || file_symbol_ids.contains(&e.target))
        // Only edges fully inside the comparable neighborhood symbol set for
        // this slice's file endpoints — caller supplies multi-file sets later
        // by comparing per-file endpoints that were both materialised.
        .filter(|e| file_symbol_ids.contains(&e.source) && file_symbol_ids.contains(&e.target))
        .map(|e| {
            (
                id_to_name
                    .get(&e.source)
                    .cloned()
                    .unwrap_or_else(|| "?".into()),
                id_to_name
                    .get(&e.target)
                    .cloned()
                    .unwrap_or_else(|| "?".into()),
                e.kind.as_str().to_string(),
            )
        })
        .collect();
    edge_keys.sort();

    FileStructuralSlice {
        symbols: sym_keys,
        references: ref_keys,
        callsites: cs_keys,
        edges: edge_keys,
    }
}

/// Cross-file edges whose endpoints lie in the given relative-path set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NeighborhoodEdgeSlice {
    edges: Vec<(String, String, String, String, String)>, // src_file, src, tgt_file, tgt, kind
}

fn neighborhood_edges(store: &Store, path_suffixes: &[&str]) -> NeighborhoodEdgeSlice {
    let files = store.list_files().unwrap();
    let mut allowed_files = std::collections::HashSet::new();
    let mut file_path: std::collections::HashMap<atlas_engine::FileId, String> =
        std::collections::HashMap::new();
    for f in &files {
        if path_suffixes.iter().any(|s| f.path.ends_with(s)) {
            allowed_files.insert(f.file_id);
            file_path.insert(f.file_id, f.path.clone());
        }
    }

    let mut id_meta: std::collections::HashMap<
        atlas_engine::SymbolId,
        (String, String), // (file_path, name)
    > = std::collections::HashMap::new();
    for fid in &allowed_files {
        for s in store.find_symbols_by_file(fid).unwrap() {
            id_meta.insert(
                s.id,
                (
                    file_path.get(fid).cloned().unwrap_or_default(),
                    s.name.clone(),
                ),
            );
        }
    }

    let mut edges: Vec<_> = store
        .get_all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| id_meta.contains_key(&e.source) && id_meta.contains_key(&e.target))
        .map(|e| {
            let (sf, sn) = id_meta.get(&e.source).unwrap();
            let (tf, tn) = id_meta.get(&e.target).unwrap();
            (
                sf.clone(),
                sn.clone(),
                tf.clone(),
                tn.clone(),
                e.kind.as_str().to_string(),
            )
        })
        .collect();
    edges.sort();
    NeighborhoodEdgeSlice { edges }
}

/// Stable dataflow/CFG slice for one function unit.
///
/// Node keys omit `arg_index`: Focus materialize (LazyDataflow) remaps callsite
/// ids after extract but does not always backfill arg_index the same way as
/// Index full; kind/name/range already identify the argument nodes.
type CfgNodeKey = (String, u32, u32);
type CfgEdgeKey = (CfgNodeKey, CfgNodeKey, String);

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnitDataflowSlice {
    nodes: Vec<(String, String, u32, u32)>, // kind, name, start, end
    edges: Vec<(usize, usize, String)>,     // indices into sorted nodes + kind
    cfg_nodes: Vec<CfgNodeKey>,             // kind, start, end
    cfg_edges: Vec<CfgEdgeKey>,
}

fn unit_dataflow_slice(store: &Store, fn_id: &atlas_engine::SymbolId) -> UnitDataflowSlice {
    let mut nodes: Vec<_> = store
        .find_data_nodes_by_function(fn_id)
        .unwrap()
        .into_iter()
        .map(|n| {
            (
                n.kind.as_str().to_string(),
                n.name.clone().unwrap_or_default(),
                n.range.start_byte,
                n.range.end_byte,
            )
        })
        .collect();
    nodes.sort();

    // Index nodes for edge endpoints (by content key, not raw id).
    let all_nodes = store.find_data_nodes_by_function(fn_id).unwrap();
    let key_of = |n: &atlas_engine::DataNode| {
        (
            n.kind.as_str().to_string(),
            n.name.clone().unwrap_or_default(),
            n.range.start_byte,
            n.range.end_byte,
        )
    };
    let mut key_to_idx: std::collections::HashMap<_, usize> = std::collections::HashMap::new();
    for (i, k) in nodes.iter().enumerate() {
        key_to_idx.insert(k.clone(), i);
    }

    let mut edges = Vec::new();
    for n in &all_nodes {
        for e in store.find_dataflow_edges_by_source(&n.id).unwrap() {
            let src_key = key_of(n);
            let tgt = all_nodes.iter().find(|x| x.id == e.target);
            let Some(tgt) = tgt else { continue };
            let tgt_key = key_of(tgt);
            if let (Some(&si), Some(&ti)) = (key_to_idx.get(&src_key), key_to_idx.get(&tgt_key)) {
                edges.push((si, ti, e.kind.as_str().to_string()));
            }
        }
    }
    edges.sort();
    edges.dedup();

    let raw_cfg_nodes = store.find_cfg_nodes_by_function(fn_id).unwrap();
    let cfg_key_of = |node: &atlas_engine::CfgNode| {
        (
            node.kind.as_str().to_string(),
            node.stmt_range.start_byte,
            node.stmt_range.end_byte,
        )
    };
    let mut cfg_nodes: Vec<_> = raw_cfg_nodes
        .iter()
        .map(|c| {
            (
                c.kind.as_str().to_string(),
                c.stmt_range.start_byte,
                c.stmt_range.end_byte,
            )
        })
        .collect();
    cfg_nodes.sort();

    let cfg_node_keys: std::collections::HashMap<_, _> = raw_cfg_nodes
        .iter()
        .map(|node| (node.id, cfg_key_of(node)))
        .collect();
    let mut cfg_edges: Vec<_> = store
        .find_cfg_edges_by_function(fn_id)
        .unwrap()
        .into_iter()
        .filter_map(|edge| {
            Some((
                cfg_node_keys.get(&edge.source)?.clone(),
                cfg_node_keys.get(&edge.target)?.clone(),
                edge.kind.as_str().to_string(),
            ))
        })
        .collect();
    cfg_edges.sort();

    UnitDataflowSlice {
        nodes,
        edges,
        cfg_nodes,
        cfg_edges,
    }
}

fn unit_binding_slice(
    store: &Store,
    fn_id: &atlas_engine::SymbolId,
) -> Vec<(String, String, u32, u32, u32)> {
    let mut bindings: Vec<_> = store
        .find_bindings_by_function(fn_id)
        .unwrap()
        .into_iter()
        .map(|binding| {
            (
                binding.kind.as_str().to_string(),
                binding.name,
                binding.visible_from_byte,
                binding.range.start_byte,
                binding.range.end_byte,
            )
        })
        .collect();
    bindings.sort();
    bindings
}

fn symbol_id_by_name(store: &Store, name: &str) -> atlas_engine::SymbolId {
    store
        .find_symbols_by_name(name)
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("symbol {name} not found"))
        .id
}

/// N5 structural: Focus ensure on seed+math matches Index structural on those
/// files; peer stays non-structural on Focus.
#[test]
fn n5_focus_structural_neighborhood_matches_index() {
    // ── Index reference DB ───────────────────────────────────────────
    let idx = setup_project(N5_NEIGHBORHOOD);
    let idx_project = idx.path().to_string_lossy().to_string();
    CommandContext::open(&idx_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&idx_project, &[], &[], &[], "structural").expect("index structural");
    let idx_store = open_store(&idx);

    // ── Focus materialize DB (manifest → ensure neighborhood) ────────
    let foc = setup_project(N5_NEIGHBORHOOD);
    let foc_project = foc.path().to_string_lossy().to_string();
    CommandContext::open(&foc_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&foc_project, &[], &[], &[], "manifest").expect("manifest only");
    let foc_store = open_store(&foc);
    let m = FocusMaterialize::open(foc_store.clone(), Some(foc.path().to_path_buf()));

    let seed_f = file_by_suffix(&foc_store, "seed.ts");
    let math_f = file_by_suffix(&foc_store, "math.ts");
    let peer_f = file_by_suffix(&foc_store, "peer.ts");

    assert!(
        !m.structural().has_structural_layer(&seed_f).unwrap(),
        "precondition: seed not structural after manifest"
    );
    assert!(
        !m.structural().has_structural_layer(&peer_f).unwrap(),
        "precondition: peer not structural after manifest"
    );

    // Materialize the call-neighborhood only (not peer) in one batch so
    // incremental resolve/graph sees both files together (cross-file Calls).
    m.structural()
        .ensure_structural_for_file_ids(&[seed_f, math_f])
        .expect("ensure seed+math structural");

    assert!(
        m.structural().has_structural_layer(&seed_f).unwrap(),
        "seed must be structural-complete after Focus materialize"
    );
    assert!(
        m.structural().has_structural_layer(&math_f).unwrap(),
        "math must be structural-complete after Focus materialize"
    );
    assert!(
        !m.structural().has_structural_layer(&peer_f).unwrap(),
        "peer must stay outside Focus structural neighborhood"
    );

    // Per-file structural slices.
    let idx_seed = file_by_suffix(&idx_store, "seed.ts");
    let idx_math = file_by_suffix(&idx_store, "math.ts");

    assert_eq!(
        structural_slice(&foc_store, &seed_f),
        structural_slice(&idx_store, &idx_seed),
        "seed.ts structural slice: Focus materialize == Index"
    );
    assert_eq!(
        structural_slice(&foc_store, &math_f),
        structural_slice(&idx_store, &idx_math),
        "math.ts structural slice: Focus materialize == Index"
    );

    // Cross-file edges inside the neighborhood (e.g. useAdd → add).
    assert_eq!(
        neighborhood_edges(&foc_store, &["seed.ts", "math.ts"]),
        neighborhood_edges(&idx_store, &["seed.ts", "math.ts"]),
        "neighborhood call/type edges: Focus == Index"
    );

    // Sanity: Index did structural-complete peer; Focus did not.
    let idx_peer = file_by_suffix(&idx_store, "peer.ts");
    // Index path uses pipeline; structural layer should exist.
    let idx_mat = FocusMaterialize::open(idx_store.clone(), Some(idx.path().to_path_buf()));
    assert!(
        idx_mat
            .structural()
            .has_structural_layer(&idx_peer)
            .unwrap(),
        "index path should structural-complete peer (whole project)"
    );
}

/// N5 dataflow: Focus ensure_for_function(seed) unit slice matches Index full
/// for the same unit; unrelated unit stays empty on Focus.
///
/// Uses a self-contained seed function (no callees) so the planner window is a
/// single unit — same shape as Full for that unit. Cross-file call expansion is
/// covered by the structural neighborhood test.
#[test]
fn n5_focus_dataflow_unit_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "seed.ts",
            "export function useAdd(x: number): number {\n\
         \x20   const y = x + 1;\n\
         \x20   return y;\n\
         }\n",
        ),
        (
            "peer.ts",
            "export function unrelated(): number {\n\
         \x20   return 99;\n\
         }\n",
        ),
    ];

    // ── Index full ───────────────────────────────────────────────────
    let idx = setup_project(FIXTURE);
    let idx_project = idx.path().to_string_lossy().to_string();
    CommandContext::open(&idx_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&idx_project, &[], &[], &[], "full").expect("index full");
    let idx_store = open_store(&idx);
    let idx_use_add = symbol_id_by_name(&idx_store, "useAdd");
    let idx_unrelated = symbol_id_by_name(&idx_store, "unrelated");
    let idx_use_slice = unit_dataflow_slice(&idx_store, &idx_use_add);
    assert!(
        !idx_use_slice.nodes.is_empty(),
        "index full must produce dataflow nodes for useAdd"
    );
    assert!(
        !unit_dataflow_slice(&idx_store, &idx_unrelated)
            .nodes
            .is_empty(),
        "index full must also dataflow-extract unrelated (whole project)"
    );

    // ── Focus: structural base + on-demand unit ensure ───────────────
    let foc = setup_project(FIXTURE);
    let foc_project = foc.path().to_string_lossy().to_string();
    CommandContext::open(&foc_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&foc_project, &[], &[], &[], "structural").expect("structural base");
    let foc_store = open_store(&foc);
    let m = FocusMaterialize::open(foc_store.clone(), Some(foc.path().to_path_buf()));

    let foc_use_add = symbol_id_by_name(&foc_store, "useAdd");
    let foc_unrelated = symbol_id_by_name(&foc_store, "unrelated");

    assert!(
        foc_store
            .find_data_nodes_by_function(&foc_use_add)
            .unwrap()
            .is_empty(),
        "precondition: no dataflow before ensure"
    );

    let window = m
        .dataflow()
        .ensure_for_function(&foc_use_add, Some("n5-parity"))
        .expect("ensure dataflow for useAdd");
    assert!(
        window.units_built >= 1 || window.units_cached >= 1,
        "ensure must build or cache the seed unit"
    );

    let foc_use_slice = unit_dataflow_slice(&foc_store, &foc_use_add);
    assert_eq!(
        foc_use_slice, idx_use_slice,
        "useAdd unit dataflow/CFG: Focus ensure == Index full"
    );

    assert!(
        foc_store
            .find_data_nodes_by_function(&foc_unrelated)
            .unwrap()
            .is_empty(),
        "unrelated unit must remain without dataflow on Focus path"
    );
}

fn baseline_language_parity_cases() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        #[cfg(feature = "typescript")]
        (
            "typescript.ts",
            "export function parityTypescript(input: number): number {\n  let local = input;\n  if (local > 0) { local = local + 1; }\n  return local;\n}\n",
            "parityTypescript",
        ),
        #[cfg(feature = "javascript")]
        (
            "javascript.js",
            "export function parityJavascript(input) {\n  let local = input;\n  if (local > 0) { local = local + 1; }\n  return local;\n}\n",
            "parityJavascript",
        ),
        #[cfg(feature = "python")]
        (
            "python.py",
            "def parity_python(input):\n    local = input\n    if local > 0:\n        local = local + 1\n    return local\n",
            "parity_python",
        ),
        #[cfg(feature = "java")]
        (
            "ParityJava.java",
            "class ParityJava {\n  static int parityJava(int input) {\n    int local = input;\n    if (local > 0) { local = local + 1; }\n    return local;\n  }\n}\n",
            "parityJava",
        ),
        #[cfg(feature = "c")]
        (
            "parity_c.c",
            "int parity_c(int input) {\n  int local = input;\n  if (local > 0) { local = local + 1; }\n  return local;\n}\n",
            "parity_c",
        ),
        #[cfg(feature = "cpp")]
        (
            "parity_cpp.cpp",
            "int parity_cpp(int input) {\n  int local = input;\n  if (local > 0) { local = local + 1; }\n  return local;\n}\n",
            "parity_cpp",
        ),
        #[cfg(feature = "arkts")]
        (
            "parity_arkts.ets",
            "function parityArkts(input: number): number {\n  let local: number = input;\n  if (local > 0) { local = local + 1; }\n  return local;\n}\n",
            "parityArkts",
        ),
        #[cfg(feature = "csharp")]
        (
            "ParityCsharp.cs",
            "class ParityCsharp {\n  static int parityCsharp(int input) {\n    int local = input;\n    if (local > 0) { local = local + 1; }\n    return local;\n  }\n}\n",
            "parityCsharp",
        ),
        #[cfg(feature = "php")]
        (
            "parity_php.php",
            "<?php\nfunction parity_php($input) {\n  $local = $input;\n  if ($local > 0) { $local = $local + 1; }\n  return $local;\n}\n",
            "parity_php",
        ),
    ]
}

/// Every language that does not already have a feature-specific N5 fixture
/// still needs a baseline product-path guard: a function materialized through
/// Focus must persist the same bindings, local dataflow, and CFG as full Index.
#[test]
fn n5_focus_baseline_language_units_match_index_full() {
    let cases = baseline_language_parity_cases();
    assert!(
        !cases.is_empty(),
        "at least one language feature is required"
    );
    let fixtures: Vec<_> = cases
        .iter()
        .map(|(path, source, _)| (*path, *source))
        .collect();

    let indexed = setup_project(&fixtures);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);

    let focused = setup_project(&fixtures);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));

    for (path, _, symbol_name) in cases {
        let indexed_symbol = symbol_id_by_name(&indexed_store, symbol_name);
        let indexed_dataflow = unit_dataflow_slice(&indexed_store, &indexed_symbol);
        let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_symbol);
        assert!(
            !indexed_dataflow.nodes.is_empty() && !indexed_dataflow.cfg_nodes.is_empty(),
            "{path}: full Index must persist dataflow and CFG"
        );
        assert!(
            !indexed_bindings.is_empty(),
            "{path}: full Index must persist function bindings"
        );

        let focused_symbol = symbol_id_by_name(&focused_store, symbol_name);
        assert!(
            focused_store
                .find_data_nodes_by_function(&focused_symbol)
                .unwrap()
                .is_empty(),
            "{path}: unit must be cold before Focus ensure"
        );
        materialize
            .dataflow()
            .ensure_for_function(&focused_symbol, Some("baseline-language-parity"))
            .unwrap_or_else(|error| panic!("{path}: Focus ensure failed: {error:#}"));

        assert_eq!(
            unit_dataflow_slice(&focused_store, &focused_symbol),
            indexed_dataflow,
            "{path}: Focus dataflow/CFG == full Index"
        );
        assert_eq!(
            unit_binding_slice(&focused_store, &focused_symbol),
            indexed_bindings,
            "{path}: Focus bindings == full Index"
        );
    }
}

fn scope_chain_language_parity_cases() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        #[cfg(feature = "typescript")]
        (
            "scope.ts",
            "function shadowTypescript(input: number): number {\n  let value = input;\n  if (input > 0) {\n    let value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
            "shadowTypescript",
        ),
        #[cfg(feature = "javascript")]
        (
            "scope.js",
            "function shadowJavascript(input) {\n  let value = input;\n  if (input > 0) {\n    let value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
            "shadowJavascript",
        ),
        #[cfg(feature = "arkts")]
        (
            "scope.ets",
            "function shadowArkts(input: number): number {\n  let value: number = input;\n  if (input > 0) {\n    let value: number = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
            "shadowArkts",
        ),
        #[cfg(feature = "c")]
        (
            "scope.c",
            "int shadow_c(int input) {\n  int value = input;\n  if (input > 0) {\n    int value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
            "shadow_c",
        ),
        #[cfg(feature = "cpp")]
        (
            "scope.cpp",
            "int shadow_cpp(int input) {\n  int value = input;\n  if (input > 0) {\n    int value = input + 1;\n    consume(value);\n  }\n  return value;\n}\n",
            "shadow_cpp",
        ),
        #[cfg(feature = "java")]
        (
            "ScopeJava.java",
            "class ScopeJava {\n  static int shadowJava(int input, boolean first) {\n    if (first) {\n      int value = input;\n      consume(value);\n    } else {\n      int value = input + 1;\n      consume(value);\n    }\n    return input;\n  }\n}\n",
            "shadowJava",
        ),
        #[cfg(feature = "go")]
        (
            "scope.go",
            "package scope\n\nfunc shadowGo(input int) int {\n  value := input\n  if input > 0 {\n    value := input + 1\n    consume(value)\n  }\n  return value\n}\n",
            "shadowGo",
        ),
        #[cfg(feature = "rust")]
        (
            "scope.rs",
            "fn shadow_rust(input: i32) -> i32 {\n  let value = input;\n  if input > 0 {\n    let value = input + 1;\n    consume(value);\n  }\n  value\n}\n",
            "shadow_rust",
        ),
        #[cfg(feature = "kotlin")]
        (
            "ScopeKotlin.kt",
            "fun shadowKotlin(input: Int): Int {\n  val value = input\n  if (input > 0) {\n    val value = input + 1\n    consume(value)\n  }\n  return value\n}\n",
            "shadowKotlin",
        ),
        #[cfg(feature = "cangjie")]
        (
            "scope.cj",
            "func shadowCangjie(input: Int64): Int64 {\n  let value = input\n  if (input > 0) {\n    let value = input + 1\n    consume(value)\n  }\n  return value\n}\n",
            "shadowCangjie",
        ),
        #[cfg(feature = "php")]
        (
            "scope.php",
            "<?php\nfunction shadowPhp($input) {\n  $value = $input;\n  $callback = function () use ($input) {\n    $value = $input + 1;\n    consume($value);\n  };\n  return $value;\n}\n",
            "shadowPhp",
        ),
        #[cfg(feature = "ruby")]
        (
            "scope.rb",
            "def shadow_ruby(input)\n  1.times do\n    value = input\n    consume(value)\n  end\n  1.times do\n    value = input + 1\n    consume(value)\n  end\n  input\nend\n",
            "shadow_ruby",
        ),
    ]
}

/// Scope-chain identity must survive both product paths. This checks the
/// persisted binding slice as well as dataflow/CFG so Focus cannot silently
/// collapse same-name locals while still matching node counts.
#[test]
fn n5_focus_scope_chain_bindings_match_index_full_across_languages() {
    let cases = scope_chain_language_parity_cases();
    assert!(
        !cases.is_empty(),
        "at least one language feature is required"
    );
    let fixtures: Vec<_> = cases
        .iter()
        .map(|(path, source, _)| (*path, *source))
        .collect();

    let indexed = setup_project(&fixtures);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);

    let focused = setup_project(&fixtures);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));

    for (path, _, symbol_name) in cases {
        let indexed_symbol = symbol_id_by_name(&indexed_store, symbol_name);
        let indexed_dataflow = unit_dataflow_slice(&indexed_store, &indexed_symbol);
        let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_symbol);
        let indexed_values: Vec<_> = indexed_store
            .find_bindings_by_function(&indexed_symbol)
            .unwrap()
            .into_iter()
            .filter(|binding| binding.name == "value")
            .collect();
        assert_eq!(indexed_values.len(), 2, "{path}: two value bindings");
        assert_ne!(indexed_values[0].id, indexed_values[1].id, "{path}");
        assert_ne!(
            indexed_values[0].scope_id, indexed_values[1].scope_id,
            "{path}"
        );

        let focused_symbol = symbol_id_by_name(&focused_store, symbol_name);
        assert!(
            focused_store
                .find_data_nodes_by_function(&focused_symbol)
                .unwrap()
                .is_empty(),
            "{path}: unit must be cold before Focus ensure"
        );
        materialize
            .dataflow()
            .ensure_for_function(&focused_symbol, Some("scope-chain-language-parity"))
            .unwrap_or_else(|error| panic!("{path}: Focus ensure failed: {error:#}"));

        assert_eq!(
            unit_dataflow_slice(&focused_store, &focused_symbol),
            indexed_dataflow,
            "{path}: Focus dataflow/CFG == full Index"
        );
        assert_eq!(
            unit_binding_slice(&focused_store, &focused_symbol),
            indexed_bindings,
            "{path}: Focus bindings == full Index"
        );
    }
}

/// C# switch pattern variables, including parenthesized nested designations,
/// are scoped to individual arms. Focus must preserve the same aggregate
/// subject-to-capture flow and binding identities as full Index without
/// materializing a peer method.
#[cfg(feature = "csharp")]
#[test]
fn n5_focus_csharp_pattern_bindings_match_index_full() {
    const FIXTURE: &[(&str, &str)] = &[(
        "PatternDispatch.cs",
        "class PatternDispatch {\n\
             \x20 static int Dispatch(object input) {\n\
             \x20   return input switch {\n\
             \x20     string value when value.Length > 0 => Consume(value),\n\
             \x20     int value => Consume(value),\n\
             \x20     var (first, (second, third)) when second != null => Consume(first, second, third),\n\
             \x20     _ => 0,\n\
             \x20   };\n\
             \x20 }\n\
             \x20 static int Peer() { return 42; }\n\
             }\n",
    )];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_dispatch = symbol_id_by_name(&indexed_store, "Dispatch");
    let indexed_slice = unit_dataflow_slice(&indexed_store, &indexed_dispatch);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_dispatch);
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| binding.0 == "local" && binding.1 == "value")
            .count(),
        2,
        "full Index must retain one value binding per switch arm"
    );
    assert_eq!(
        indexed_slice
            .nodes
            .iter()
            .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "value")
            .count(),
        2,
        "full Index must retain both pattern capture events"
    );
    for name in ["first", "second", "third"] {
        assert_eq!(
            indexed_bindings
                .iter()
                .filter(|binding| binding.0 == "local" && binding.1 == name)
                .count(),
            1,
            "full Index must retain nested designation {name}"
        );
        assert_eq!(
            indexed_slice
                .nodes
                .iter()
                .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == name)
                .count(),
            1,
            "full Index must retain nested designation target {name}"
        );
    }
    let indexed_nodes = indexed_store
        .find_data_nodes_by_function(&indexed_dispatch)
        .unwrap();
    let indexed_subject = indexed_nodes
        .iter()
        .find(|node| node.kind == DataNodeKind::Expr && node.name.as_deref() == Some("input"))
        .expect("full Index switch subject");
    let indexed_third = indexed_nodes
        .iter()
        .find(|node| node.kind == DataNodeKind::Local && node.name.as_deref() == Some("third"))
        .expect("full Index third target");
    let indexed_third_edge = indexed_store
        .find_dataflow_edges_by_source(&indexed_subject.id)
        .unwrap()
        .into_iter()
        .find(|edge| edge.target == indexed_third.id && edge.kind == DataFlowKind::Assign)
        .expect("full Index aggregate subject flow to third");
    assert_eq!(indexed_third_edge.confidence, 0.72);

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let dispatch = symbol_id_by_name(&focused_store, "Dispatch");
    let peer = symbol_id_by_name(&focused_store, "Peer");
    assert!(
        focused_store
            .find_data_nodes_by_function(&dispatch)
            .unwrap()
            .is_empty(),
        "C# pattern unit must be cold before Focus ensure"
    );

    materialize
        .dataflow()
        .ensure_for_function(&dispatch, Some("csharp-pattern-parity"))
        .expect("Focus ensure C# Dispatch");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &dispatch),
        indexed_slice,
        "C# pattern dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &dispatch),
        indexed_bindings,
        "C# pattern bindings: Focus ensure == Index full"
    );
    let focused_nodes = focused_store
        .find_data_nodes_by_function(&dispatch)
        .unwrap();
    let focused_subject = focused_nodes
        .iter()
        .find(|node| node.kind == DataNodeKind::Expr && node.name.as_deref() == Some("input"))
        .expect("Focus switch subject");
    let focused_third = focused_nodes
        .iter()
        .find(|node| node.kind == DataNodeKind::Local && node.name.as_deref() == Some("third"))
        .expect("Focus third target");
    let focused_third_edge = focused_store
        .find_dataflow_edges_by_source(&focused_subject.id)
        .unwrap()
        .into_iter()
        .find(|edge| edge.target == focused_third.id && edge.kind == DataFlowKind::Assign)
        .expect("Focus aggregate subject flow to third");
    assert_eq!(focused_third_edge.confidence, 0.72);
    assert!(
        focused_store
            .find_data_nodes_by_function(&peer)
            .unwrap()
            .is_empty(),
        "peer C# method must stay outside the Focus window"
    );
}

/// PHP foreach key/value declarations use the callable variable namespace,
/// not the structural loop/block scopes. Focus and full Index must retain the
/// same post-loop binding/dataflow facts without warming an unrelated unit.
#[cfg(feature = "php")]
#[test]
fn n5_focus_php_foreach_namespace_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "foreach_scope.php",
            "<?php\n\
             function iterate($items) {\n\
             \x20 foreach ($items as $key => $value) {\n\
             \x20   consume($value);\n\
             \x20 }\n\
             \x20 return $value + $key;\n\
             }\n",
        ),
        ("peer.php", "<?php\nfunction unrelated() { return 42; }\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_iterate = symbol_id_by_name(&indexed_store, "iterate");
    let indexed_slice = unit_dataflow_slice(&indexed_store, &indexed_iterate);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_iterate);
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| matches!(binding.1.as_str(), "items" | "key" | "value"))
            .count(),
        3,
        "full Index must persist the parameter plus foreach key/value bindings"
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let iterate = symbol_id_by_name(&focused_store, "iterate");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");
    assert!(
        focused_store
            .find_data_nodes_by_function(&iterate)
            .unwrap()
            .is_empty(),
        "PHP foreach unit must be cold before Focus ensure"
    );

    materialize
        .dataflow()
        .ensure_for_function(&iterate, Some("php-foreach-namespace-parity"))
        .expect("Focus ensure PHP iterate");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &iterate),
        indexed_slice,
        "PHP foreach dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &iterate),
        indexed_bindings,
        "PHP foreach callable bindings: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated PHP unit must stay outside the Focus window"
    );
}

/// PHP nested/keyed destructuring must persist the same callable bindings and
/// conservative aggregate flow through Focus as through a full Index. The key
/// selector remains a parameter read, and an unrelated PHP unit stays cold.
#[cfg(feature = "php")]
#[test]
fn n5_focus_php_nested_destructuring_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "nested_destructure.php",
            "<?php\n\
             function unpack($source, $rows, $selector) {\n\
             \x20 list($first, list($second, &$third)) = $source;\n\
             \x20 [$selector => $selected] = $source;\n\
             \x20 foreach ($rows as ['meta' => ['flag' => $row_flag]]) {\n\
             \x20   consume($row_flag);\n\
             \x20 }\n\
             \x20 return consume($first, $second, $third, $selected);\n\
             }\n",
        ),
        ("peer.php", "<?php\nfunction unrelated() { return 42; }\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_unpack = symbol_id_by_name(&indexed_store, "unpack");
    let indexed_dataflow = unit_dataflow_slice(&indexed_store, &indexed_unpack);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_unpack);
    for name in [
        "source", "rows", "selector", "first", "second", "third", "selected", "row_flag",
    ] {
        assert_eq!(
            indexed_store
                .find_bindings_by_function(&indexed_unpack)
                .unwrap()
                .iter()
                .filter(|binding| binding.name == name)
                .count(),
            1,
            "{name} must have one binding identity in the full Index"
        );
    }

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let unpack = symbol_id_by_name(&focused_store, "unpack");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");
    assert!(
        focused_store
            .find_data_nodes_by_function(&unpack)
            .unwrap()
            .is_empty(),
        "PHP destructuring unit must be cold before Focus ensure"
    );

    materialize
        .dataflow()
        .ensure_for_function(&unpack, Some("php-nested-destructuring-parity"))
        .expect("Focus ensure PHP nested destructuring");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &unpack),
        indexed_dataflow,
        "PHP destructuring dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &unpack),
        indexed_bindings,
        "PHP destructuring bindings: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated PHP unit must stay outside the Focus window"
    );
}

/// Go type-switch aliases are clause-local implicit bindings. Full Index and
/// Focus must persist the same three binding identities and guard-value flow
/// for the standard-library `context.stringify` shape.
#[cfg(feature = "go")]
#[test]
fn n5_focus_go_type_switch_alias_dataflow_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "context_stringify.go",
            "package context\n\
             \n\
             type stringer interface { String() string }\n\
             \n\
             func stringify(v any) string {\n\
             \x20 switch s := v.(type) {\n\
             \x20 case stringer:\n\
             \x20   return s.String()\n\
             \x20 case string:\n\
             \x20   return s\n\
             \x20 case nil:\n\
             \x20   return \"<nil>\"\n\
             \x20 }\n\
             \x20 return typeName(v)\n\
             }\n",
        ),
        (
            "peer.go",
            "package context\nfunc unrelated() int { return 42 }\n",
        ),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_stringify = symbol_id_by_name(&indexed_store, "stringify");
    let indexed_slice = unit_dataflow_slice(&indexed_store, &indexed_stringify);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_stringify);
    assert_eq!(
        indexed_slice
            .nodes
            .iter()
            .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "s")
            .count(),
        3,
        "full Index must keep one Local alias event per type case"
    );
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| binding.0 == "local" && binding.1 == "s")
            .count(),
        3,
        "full Index must keep one alias BindingDef per implicit case block"
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let stringify = symbol_id_by_name(&focused_store, "stringify");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");

    materialize
        .dataflow()
        .ensure_for_function(&stringify, Some("go-type-switch-alias-parity"))
        .expect("Focus ensure Go stringify");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &stringify),
        indexed_slice,
        "Go type-switch dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &stringify),
        indexed_bindings,
        "Go type-switch clause bindings: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Go unit must stay outside the Focus window"
    );
}

/// Go mixed short declarations must preserve the same canonical bindings on
/// the cold Focus path as on a full Index, including the function-body
/// parameter exception and nested-block shadowing.
#[cfg(feature = "go")]
#[test]
fn n5_focus_go_mixed_short_declaration_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "mixed_short.go",
            "package p\n\
             \n\
             func mixed(input int) int {\n\
             \x20 input, extra := input + 1, 2\n\
             \x20 extra, value := extra + 1, input\n\
             \x20 if input > 0 {\n\
             \x20   value, nested := value + 1, input\n\
             \x20   consume(value, nested)\n\
             \x20 }\n\
             \x20 consume(input, extra, value)\n\
             \x20 return value\n\
             }\n",
        ),
        ("peer.go", "package p\nfunc unrelated() int { return 42 }\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_mixed = symbol_id_by_name(&indexed_store, "mixed");
    let indexed_dataflow = unit_dataflow_slice(&indexed_store, &indexed_mixed);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_mixed);
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| binding.1 == "input")
            .count(),
        1,
        "parameter redeclaration must retain one canonical binding"
    );
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| binding.1 == "extra")
            .count(),
        1,
        "same-block local redeclaration must retain one canonical binding"
    );
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| binding.1 == "value")
            .count(),
        2,
        "nested block must still introduce a shadow binding"
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let mixed = symbol_id_by_name(&focused_store, "mixed");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");
    assert!(
        focused_store
            .find_data_nodes_by_function(&mixed)
            .unwrap()
            .is_empty(),
        "mixed unit must be cold before Focus ensure"
    );

    materialize
        .dataflow()
        .ensure_for_function(&mixed, Some("go-mixed-short-declaration-parity"))
        .expect("Focus ensure Go mixed declaration");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &mixed),
        indexed_dataflow,
        "Go mixed declaration dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &mixed),
        indexed_bindings,
        "Go mixed declaration bindings: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Go unit must stay outside the Focus window"
    );
}

/// Ruby modifier-loop CFG must be identical whether the unit comes from a full
/// Index or Focus on-demand materialization. This protects the pre-test vs
/// `begin ... end` post-test entry ordering across both product paths.
#[cfg(feature = "ruby")]
#[test]
fn n5_focus_ruby_modifier_cfg_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "modifier.rb",
            "def run(ready, done)\n\
             \x20 work() while ready\n\
             \x20 begin\n\
             \x20   work()\n\
             \x20 end until done\n\
             \x20 after()\n\
             end\n",
        ),
        ("peer.rb", "def unrelated\n  42\nend\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_slice =
        unit_dataflow_slice(&indexed_store, &symbol_id_by_name(&indexed_store, "run"));

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let run = symbol_id_by_name(&focused_store, "run");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");

    materialize
        .dataflow()
        .ensure_for_function(&run, Some("ruby-modifier-parity"))
        .expect("Focus ensure Ruby run");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &run),
        indexed_slice,
        "Ruby modifier-loop dataflow/CFG edges: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_cfg_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Ruby unit must stay outside the Focus window"
    );
}

/// Ruby case/in must retain its implicit no-match Throw and outer-loop break
/// target identically across full Index and Focus on-demand dataflow.
#[cfg(feature = "ruby")]
#[test]
fn n5_focus_ruby_case_in_cfg_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "case_in.rb",
            "def dispatch(active, value)\n\
             \x20 while active\n\
             \x20   case value\n\
             \x20   in {kind: \"stop\"}\n\
             \x20     break\n\
             \x20   in [head, *tail]\n\
             \x20     consume(head)\n\
             \x20   end\n\
             \x20   after_case()\n\
             \x20 end\n\
             \x20 after_loop()\n\
             end\n",
        ),
        ("peer.rb", "def unrelated\n  42\nend\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_slice = unit_dataflow_slice(
        &indexed_store,
        &symbol_id_by_name(&indexed_store, "dispatch"),
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let dispatch = symbol_id_by_name(&focused_store, "dispatch");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");

    materialize
        .dataflow()
        .ensure_for_function(&dispatch, Some("ruby-case-in-parity"))
        .expect("Focus ensure Ruby dispatch");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &dispatch),
        indexed_slice,
        "Ruby case/in dataflow/CFG edges: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_cfg_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Ruby unit must stay outside the Focus window"
    );
}

/// Ruby multiple assignment must materialize identical binding identities and
/// positional/aggregate dataflow through Focus and a full Index. In particular,
/// a block write reuses an earlier method local while new block names stay local.
#[cfg(feature = "ruby")]
#[test]
fn n5_focus_ruby_multiple_assignment_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "multiple_assignment.rb",
            "def unpack(pair, tail)\n\
             \x20 first, second = pair\n\
             \x20 1.times do\n\
             \x20   first, block_only = second, tail\n\
             \x20   inner, *rest = pair\n\
             \x20   consume(first, block_only, inner, rest)\n\
             \x20 end\n\
             \x20 consume(first, second)\n\
             \x20 first\n\
             end\n",
        ),
        ("peer.rb", "def unrelated\n  42\nend\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_unpack = symbol_id_by_name(&indexed_store, "unpack");
    let indexed_dataflow = unit_dataflow_slice(&indexed_store, &indexed_unpack);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_unpack);
    for name in [
        "pair",
        "tail",
        "first",
        "second",
        "block_only",
        "inner",
        "rest",
    ] {
        assert_eq!(
            indexed_store
                .find_bindings_by_function(&indexed_unpack)
                .unwrap()
                .iter()
                .filter(|binding| binding.name == name)
                .count(),
            1,
            "{name} must have one binding identity in the full Index"
        );
    }

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let unpack = symbol_id_by_name(&focused_store, "unpack");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");
    assert!(
        focused_store
            .find_data_nodes_by_function(&unpack)
            .unwrap()
            .is_empty(),
        "unpack unit must be cold before Focus ensure"
    );

    materialize
        .dataflow()
        .ensure_for_function(&unpack, Some("ruby-multiple-assignment-parity"))
        .expect("Focus ensure Ruby multiple assignment");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &unpack),
        indexed_dataflow,
        "Ruby multiple-assignment dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &unpack),
        indexed_bindings,
        "Ruby multiple-assignment bindings: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Ruby unit must stay outside the Focus window"
    );
}

/// Kotlin `when (val subject = initializer)` must materialize the same
/// initializer→subject binding flow and guarded sibling CFG through Focus as
/// through a full Index, without touching unrelated units.
#[cfg(feature = "kotlin")]
#[test]
fn n5_focus_kotlin_when_subject_dataflow_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "when_subject.kt",
            "fun dispatch(source: Source): String {\n\
             \x20 return when (val result = source.load()) {\n\
             \x20   is Success if result.ready -> consume(result)\n\
             \x20   result -> echo(result)\n\
             \x20   is Failure -> fail(result.error)\n\
             \x20   else -> fallback(result)\n\
             \x20 }\n\
             }\n",
        ),
        ("peer.kt", "fun unrelated(): Int = 42\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_slice = unit_dataflow_slice(
        &indexed_store,
        &symbol_id_by_name(&indexed_store, "dispatch"),
    );
    assert!(
        indexed_slice
            .nodes
            .iter()
            .any(|node| { node.0 == DataNodeKind::Local.as_str() && node.1 == "result" })
    );
    assert!(
        indexed_slice
            .edges
            .iter()
            .any(|edge| edge.2 == DataFlowKind::Assign.as_str())
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let dispatch = symbol_id_by_name(&focused_store, "dispatch");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");

    materialize
        .dataflow()
        .ensure_for_function(&dispatch, Some("kotlin-when-subject-parity"))
        .expect("Focus ensure Kotlin dispatch");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &dispatch),
        indexed_slice,
        "Kotlin when-subject dataflow/CFG: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Kotlin unit must stay outside the Focus window"
    );
}

/// Kotlin nested locals with the same name must retain two scope-chain
/// identities, and Focus must materialize the same binding/dataflow unit as a
/// full Index without warming an unrelated function.
#[cfg(feature = "kotlin")]
#[test]
fn n5_focus_kotlin_nested_local_shadowing_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "shadow.kt",
            "fun shadow(input: Int): Int {\n\
             \x20 val value = input\n\
             \x20 if (input > 0) {\n\
             \x20   val value = input + 1\n\
             \x20   consume(value)\n\
             \x20 }\n\
             \x20 return value\n\
             }\n",
        ),
        ("peer.kt", "fun unrelated(): Int = 42\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_shadow = symbol_id_by_name(&indexed_store, "shadow");
    let indexed_slice = unit_dataflow_slice(&indexed_store, &indexed_shadow);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_shadow);
    assert_eq!(
        indexed_slice
            .nodes
            .iter()
            .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "value")
            .count(),
        2,
        "full Index must keep both value declaration events"
    );
    assert_eq!(
        indexed_bindings
            .iter()
            .filter(|binding| binding.0 == "local" && binding.1 == "value")
            .count(),
        2,
        "full Index must keep both scoped value bindings"
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let shadow = symbol_id_by_name(&focused_store, "shadow");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");
    assert!(
        focused_store
            .find_data_nodes_by_function(&shadow)
            .unwrap()
            .is_empty(),
        "Kotlin shadow unit must be cold before Focus ensure"
    );

    materialize
        .dataflow()
        .ensure_for_function(&shadow, Some("kotlin-shadowing-parity"))
        .expect("Focus ensure Kotlin shadow");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &shadow),
        indexed_slice,
        "Kotlin shadowing dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &shadow),
        indexed_bindings,
        "Kotlin shadowing bindings: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Kotlin unit must stay outside the Focus window"
    );
}

/// Cangjie match capture bindings and guard/body identity must produce the
/// same dataflow/CFG facts through Focus as through a full Index.
#[cfg(feature = "cangjie")]
#[test]
fn n5_focus_cangjie_match_binding_dataflow_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "match_bindings.cj",
            "enum Result {\n\
             \x20 | Success(Int64)\n\
             \x20 | Failure(Int64)\n\
             }\n\
             func dispatch(value: Result): Int64 {\n\
             \x20 return match (value) {\n\
             \x20   case Success(payload) where payload > 0 => consume(payload)\n\
             \x20   case Failure(payload) => consume(payload)\n\
             \x20   case fallback => consume(fallback)\n\
             \x20 }\n\
             }\n",
        ),
        ("peer.cj", "func unrelated(): Int64 {\n    return 42\n}\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_slice = unit_dataflow_slice(
        &indexed_store,
        &symbol_id_by_name(&indexed_store, "dispatch"),
    );
    assert_eq!(
        indexed_slice
            .nodes
            .iter()
            .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "payload")
            .count(),
        2
    );
    assert!(
        indexed_slice
            .edges
            .iter()
            .any(|edge| edge.2 == DataFlowKind::Assign.as_str())
    );

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let dispatch = symbol_id_by_name(&focused_store, "dispatch");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");

    materialize
        .dataflow()
        .ensure_for_function(&dispatch, Some("cangjie-match-binding-parity"))
        .expect("Focus ensure Cangjie dispatch");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &dispatch),
        indexed_slice,
        "Cangjie match binding dataflow/CFG: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Cangjie unit must stay outside the Focus window"
    );
}

/// Rust match captures and guard/body identity must produce the same
/// dataflow/CFG facts through Focus as through a full Index.
#[cfg(feature = "rust")]
#[test]
fn n5_focus_rust_match_binding_dataflow_matches_index_full() {
    const FIXTURE: &[(&str, &str)] = &[
        (
            "match_bindings.rs",
            "enum Result {\n\
             \x20   Good(i32),\n\
             \x20   Bad(i32),\n\
             }\n\
             fn dispatch(value: Result, fallback: Option<i32>) -> i32 {\n\
             \x20   match value {\n\
             \x20       Result::Good(payload) if let Some(extra) = fallback && extra > payload => consume(payload) + extra,\n\
             \x20       Result::Bad(payload) => consume(payload),\n\
             \x20   }\n\
             }\n",
        ),
        ("peer.rs", "fn unrelated() -> i32 {\n    42\n}\n"),
    ];

    let indexed = setup_project(FIXTURE);
    let indexed_project = indexed.path().to_string_lossy().to_string();
    CommandContext::open(&indexed_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&indexed_project, &[], &[], &[], "full").expect("index full");
    let indexed_store = open_store(&indexed);
    let indexed_dispatch = symbol_id_by_name(&indexed_store, "dispatch");
    let indexed_slice = unit_dataflow_slice(&indexed_store, &indexed_dispatch);
    let indexed_bindings = unit_binding_slice(&indexed_store, &indexed_dispatch);
    assert_eq!(
        indexed_slice
            .nodes
            .iter()
            .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "payload")
            .count(),
        2
    );
    assert_eq!(
        indexed_slice
            .nodes
            .iter()
            .filter(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "extra")
            .count(),
        1
    );
    let fallback_rhs = indexed_slice
        .nodes
        .iter()
        .position(|node| node.0 == DataNodeKind::Expr.as_str() && node.1 == "fallback")
        .expect("guard-let RHS in full Index");
    let extra_target = indexed_slice
        .nodes
        .iter()
        .position(|node| node.0 == DataNodeKind::Local.as_str() && node.1 == "extra")
        .expect("guard-let target in full Index");
    assert!(indexed_slice.edges.iter().any(|edge| {
        edge.0 == fallback_rhs && edge.1 == extra_target && edge.2 == DataFlowKind::Assign.as_str()
    }));

    let focused = setup_project(FIXTURE);
    let focused_project = focused.path().to_string_lossy().to_string();
    CommandContext::open(&focused_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&focused_project, &[], &[], &[], "structural").expect("structural base");
    let focused_store = open_store(&focused);
    let materialize =
        FocusMaterialize::open(focused_store.clone(), Some(focused.path().to_path_buf()));
    let dispatch = symbol_id_by_name(&focused_store, "dispatch");
    let unrelated = symbol_id_by_name(&focused_store, "unrelated");

    materialize
        .dataflow()
        .ensure_for_function(&dispatch, Some("rust-match-binding-parity"))
        .expect("Focus ensure Rust dispatch");
    assert_eq!(
        unit_dataflow_slice(&focused_store, &dispatch),
        indexed_slice,
        "Rust match binding dataflow/CFG: Focus ensure == Index full"
    );
    assert_eq!(
        unit_binding_slice(&focused_store, &dispatch),
        indexed_bindings,
        "Rust guard-let binding activation: Focus ensure == Index full"
    );
    assert!(
        focused_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "unrelated Rust unit must stay outside the Focus window"
    );
}

/// N5 dataflow with planner call expansion: ensure(useAdd) also builds callee
/// `add`; both units match Index full; peer stays empty.
#[test]
fn n5_focus_dataflow_expanded_window_matches_index_full() {
    // ── Index full ───────────────────────────────────────────────────
    let idx = setup_project(N5_NEIGHBORHOOD);
    let idx_project = idx.path().to_string_lossy().to_string();
    CommandContext::open(&idx_project, DbMode::InitOrCreate).expect("init index db");
    index::run(&idx_project, &[], &[], &[], "full").expect("index full");
    let idx_store = open_store(&idx);
    let idx_use = unit_dataflow_slice(&idx_store, &symbol_id_by_name(&idx_store, "useAdd"));
    let idx_add = unit_dataflow_slice(&idx_store, &symbol_id_by_name(&idx_store, "add"));
    assert!(!idx_use.nodes.is_empty(), "index full: useAdd has dataflow");
    assert!(!idx_add.nodes.is_empty(), "index full: add has dataflow");

    // ── Focus structural + ensure useAdd (window expands to add) ─────
    let foc = setup_project(N5_NEIGHBORHOOD);
    let foc_project = foc.path().to_string_lossy().to_string();
    CommandContext::open(&foc_project, DbMode::InitOrCreate).expect("init focus db");
    index::run(&foc_project, &[], &[], &[], "structural").expect("structural base");
    let foc_store = open_store(&foc);
    let m = FocusMaterialize::open(foc_store.clone(), Some(foc.path().to_path_buf()));

    let use_add = symbol_id_by_name(&foc_store, "useAdd");
    let add = symbol_id_by_name(&foc_store, "add");
    let unrelated = symbol_id_by_name(&foc_store, "unrelated");

    let window = m
        .dataflow()
        .ensure_for_function(&use_add, Some("n5-expand"))
        .expect("ensure useAdd");
    assert!(
        window.units.len() >= 2,
        "planner should expand to callee unit(s), got {} units",
        window.units.len()
    );

    assert_eq!(
        unit_dataflow_slice(&foc_store, &use_add),
        idx_use,
        "useAdd (seed) unit: Focus expanded window == Index full"
    );
    assert_eq!(
        unit_dataflow_slice(&foc_store, &add),
        idx_add,
        "add (callee) unit: Focus expanded window == Index full"
    );
    assert!(
        foc_store
            .find_data_nodes_by_function(&unrelated)
            .unwrap()
            .is_empty(),
        "peer unrelated must not receive dataflow from useAdd ensure"
    );
}

/// N5 layer-2: FocusRuntime.prepare materializes call-neighborhood structural
/// facts; peer file stays outside foreground structural complete set.
#[test]
fn n5_focus_runtime_prepare_structural_neighborhood() {
    let foc = setup_project(N5_NEIGHBORHOOD);
    let foc_project = foc.path().to_string_lossy().to_string();
    CommandContext::open(&foc_project, DbMode::InitOrCreate).expect("init");
    // Cold-ish start: inventory + top-level only — Focus must strengthen.
    index::run(&foc_project, &[], &[], &[], "manifest").expect("manifest");
    let foc_store = open_store(&foc);
    let m = FocusMaterialize::open(foc_store.clone(), Some(foc.path().to_path_buf()));

    let seed_f = file_by_suffix(&foc_store, "seed.ts");
    let math_f = file_by_suffix(&foc_store, "math.ts");
    let peer_f = file_by_suffix(&foc_store, "peer.ts");
    assert!(!m.structural().has_structural_layer(&seed_f).unwrap());
    assert!(!m.structural().has_structural_layer(&peer_f).unwrap());

    let mut rt = FocusRuntime::new(foc_store.clone(), Some(foc.path().to_path_buf()), m.clone());
    let intent = QueryIntent::Calls {
        symbol_name: "useAdd".to_string(),
        file_id: Some(seed_f),
        symbol_id: None,
        direction: Some("outgoing".to_string()),
        depth: Some(1),
    };
    let result = rt
        .prepare(&intent, Vec::new())
        .expect("FocusRuntime::prepare");
    assert_eq!(
        result.access,
        AccessStrategy::Focus,
        "manifest-only project must take Focus path"
    );
    assert!(
        result.closure_id.is_some(),
        "prepare should open a Focus closure"
    );

    // Foreground materialize must cover seed; call expansion typically pulls math.
    assert!(
        m.structural().has_structural_layer(&seed_f).unwrap()
            || result.built_files.contains(&seed_f)
            || result.seed_file_id == Some(seed_f),
        "seed file must be structural-complete or recorded as seed/built after prepare"
    );
    // If math was built (outgoing calls depth 1), it must match Index structural slice.
    if m.structural().has_structural_layer(&math_f).unwrap() {
        let idx = setup_project(N5_NEIGHBORHOOD);
        let idx_project = idx.path().to_string_lossy().to_string();
        CommandContext::open(&idx_project, DbMode::InitOrCreate).expect("init idx");
        index::run(&idx_project, &[], &[], &[], "structural").expect("index structural");
        let idx_store = open_store(&idx);
        let idx_seed = file_by_suffix(&idx_store, "seed.ts");
        let idx_math = file_by_suffix(&idx_store, "math.ts");
        if m.structural().has_structural_layer(&seed_f).unwrap() {
            assert_eq!(
                structural_slice(&foc_store, &seed_f),
                structural_slice(&idx_store, &idx_seed),
                "prepare seed structural == Index"
            );
        }
        assert_eq!(
            structural_slice(&foc_store, &math_f),
            structural_slice(&idx_store, &idx_math),
            "prepare math structural == Index"
        );
    }

    assert!(
        !m.structural().has_structural_layer(&peer_f).unwrap(),
        "peer must not be structural-complete after prepare(useAdd) — not file-wide fan-out"
    );
}
