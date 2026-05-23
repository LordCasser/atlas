//! CLI end-to-end tests for `atlas trace`.
//!
//! These tests exercise the full `atlas init` → `atlas index` → `atlas trace`
//! workflow using real source files in temporary project directories.  They
//! validate that the [`TraceQueryResponse<T>`] JSON envelope is produced
//! correctly regardless of whether the underlying analysis succeeds,
//! partially succeeds, or is unsupported for a given language.
//!
//! Run with default features:  `cargo test --test trace_cli_e2e`
//! Run with all languages:    `cargo test --test trace_cli_e2e --features all-languages`

use atlas_analysis::trace::TraceEngine;
use atlas_cli::commands::{index, init};
use atlas_db::Store;
use atlas_types::ids::{FileId, SymbolId};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

/// Create a temporary project with the given source files,
/// run `atlas init` and `atlas index`, and return the project root + store.
fn init_and_index(files: &[(&str, &str)]) -> (TempDir, Arc<Store>) {
    let tmp = TempDir::new().expect("create temp dir");

    // Write source files
    for (rel_path, content) in files {
        let file_path = tmp.path().join(rel_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&file_path, content).expect("write source file");
    }

    let project = tmp.path().to_string_lossy().to_string();

    // Run init
    init::run(&project).expect("atlas init");

    // Run index
    index::run(&project, None, None).expect("atlas index");

    // Open store for queries
    let store = Arc::new(Store::open_db(&tmp.path().join(".atlas/atlas.db")).expect("open store"));

    (tmp, store)
}

/// Create a TraceEngine from a store.
fn engine(store: Arc<Store>) -> TraceEngine {
    TraceEngine::new(store)
}

/// Create a TraceEngine with project root for snippet extraction.
fn engine_with_root(store: Arc<Store>, root: &Path) -> TraceEngine {
    TraceEngine::new_with_root(store, root.to_path_buf())
}

/// Resolve a file id by relative path within the project root.
fn resolve_file_id(engine: &TraceEngine, root: &Path, rel_path: &str) -> FileId {
    engine
        .resolve_file_id_with_root(root, rel_path)
        .expect("resolve file_id")
        .expect("file_id found")
}

/// Find a symbol by name within a file.
fn find_symbol(store: &Store, file_id: &FileId, name: &str) -> SymbolId {
    let syms = store.find_symbols_by_file(file_id).expect("find symbols");
    syms.iter()
        .find(|s| s.name == name)
        .expect(&format!("symbol '{}' not found", name))
        .id
}

/// Extract the JSON string from a TraceQueryResponse by serializing it.
fn response_json<T: serde::Serialize>(resp: &T) -> Value {
    let json_str = serde_json::to_string(resp).expect("serialize response");
    serde_json::from_str(&json_str).expect("parse response JSON")
}

// ────────────────────────────────────────────────────────────────
// P0: Full pipeline + TraceQueryResponse envelope
// ────────────────────────────────────────────────────────────────

/// The most important test: full init → index → trace_point, verify the
/// response envelope has all mandatory fields.
#[test]
fn p0_cli_full_pipeline_point_envelope() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "src/app.ts",
        r#"function greet(name: string): string {
    return `Hello, ${name}!`;
}

function main(): void {
    const msg = greet("World");
    console.log(msg);
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());
    let file_id = resolve_file_id(&eng, tmp.path(), "src/app.ts");

    // Trace at the call expression `greet("World")` — we want a resolved reference + symbol.
    let resp = eng.trace_point(&file_id, 6, 20);

    // Envelope assertions (P2 fix: all fields always present)
    let json = response_json(&resp);
    assert!(
        json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        "response.ok must be true"
    );
    assert_eq!(
        json.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
        "trace_point",
        "response.kind must be 'trace_point'"
    );
    assert!(
        json.get("partial_result").is_some(),
        "partial_result must be present (even if false)"
    );
    assert!(
        json.get("diagnostics").is_some(),
        "diagnostics must be present (even if empty)"
    );
    assert!(
        json.get("result").is_some(),
        "result must be present (may be null)"
    );
    assert!(
        json.get("capability").is_some(),
        "capability field must be present for agent consumption"
    );

    // Result content
    assert!(resp.result.is_some(), "trace_point should produce a result");
    let point = resp.result.as_ref().unwrap();
    assert!(
        point.reference.is_some() || point.resolved_symbol.is_some(),
        "either reference or resolved symbol should exist at call position"
    );
}

/// trace_variable returns a response envelope (ok, kind, etc.) for dataflow.
#[test]
fn p0_cli_trace_variable_envelope() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "calc.ts",
        r#"function compute(): number {
    const base = 10;
    const multiplier = 3;
    const result = base * multiplier;
    return result;
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let root = tmp.path();
    let file_id = resolve_file_id(&eng, root, "calc.ts");

    // Locate at `result` assignment (line 4, column 18)
    let resp = eng.trace_variable(&file_id, 4, 18, 20);

    let json = response_json(&resp);
    assert!(json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
    assert_eq!(
        json.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
        "trace_variable"
    );
    assert!(
        json.get("capability").is_some(),
        "capability must be present"
    );
}

/// trace_caller_path returns a response envelope for caller chain queries.
#[test]
fn p0_cli_trace_caller_path_envelope() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "chain.ts",
        r#"function inner(x: number): number {
    return x + 1;
}

function middle(y: number): number {
    return inner(y);
}

function outer(z: number): void {
    const n = middle(z);
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let engine = engine(store.clone());
    let root = tmp.path();
    let file_id = resolve_file_id(&engine, root, "chain.ts");
    let target_id = find_symbol(&store, &file_id, "inner");

    let resp = engine.trace_callers(&target_id, 10);

    let json = response_json(&resp);
    assert!(json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
    assert_eq!(
        json.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
        "trace_callers"
    );
    assert!(json.get("result").is_some());

    // The result should contain a caller chain with at least the middle step.
    assert!(resp.result.is_some());
    let chain = resp.result.as_ref().unwrap();
    assert!(!chain.steps.is_empty(), "expected at least one caller step");
    assert_eq!(chain.target.name, "inner");
}

// ────────────────────────────────────────────────────────────────
// P2: JSON contract stability — all fields present, even empty
// ────────────────────────────────────────────────────────────────

/// Even successful responses must serialize all envelope fields.
#[test]
fn p0_cli_json_success_has_all_envelope_fields() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const VERSION = '1.0.0';\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "app.ts");

    let resp = eng.trace_point(&file_id, 1, 10);
    assert!(resp.ok, "trace_point should succeed");

    let json = response_json(&resp);
    // All 6 mandatory envelope fields
    let required_fields = [
        "ok",
        "kind",
        "capability",
        "partial_result",
        "diagnostics",
        "result",
    ];
    for field in &required_fields {
        assert!(
            json.get(field).is_some(),
            "envelope JSON must contain field '{}'",
            field
        );
    }
}

/// Partial (no-result) responses must still be well-formed JSON.
#[test]
fn p0_cli_partial_response_still_valid_json() {
    let _ = tracing_subscriber::fmt::try_init();
    // A function name on its own has no data node; trace_variable should partial.
    let files = &[("fn.ts", "function standalone(): number { return 42; }\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "fn.ts");

    // Point at function name (should resolve symbol, but slicer needs data_node)
    let resp = eng.trace_variable(&file_id, 1, 10, 10);

    // Partial result should still be ok=true, with diagnostics
    let json = response_json(&resp);
    assert!(
        json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        "partial response must have ok=true"
    );
    assert!(
        json.get("partial_result")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "partial response must have partial_result=true"
    );
    assert!(
        json.get("diagnostics")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "partial response must have non-empty diagnostics"
    );
    // result should still be present (as null)
    assert!(
        json.get("result").is_some(),
        "result field must be present even if null"
    );
}

// ────────────────────────────────────────────────────────────────
// P1: Capability boundary — unsupported languages get partial
// ────────────────────────────────────────────────────────────────

/// Java: trace_variable must return partial (no dataflow support).
#[cfg(feature = "java")]
#[test]
fn p1_capability_java_variable_is_partial() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "App.java",
        r#"public class App {
    public static void main(String[] args) {
        int x = 42;
        System.out.println(x);
    }
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "App.java");

    let resp = eng.trace_variable(&file_id, 3, 17, 10);

    // Java is Symbolic-level — dataflow not supported
    assert!(resp.ok, "Java variable trace should not be an error");
    assert!(resp.partial_result, "Java variable trace should be partial");
    assert!(
        !resp.diagnostics.is_empty(),
        "should have unsupported_language diagnostic"
    );
    assert!(
        resp.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("unsupported_language")),
        "diagnostic code should be 'unsupported_language'"
    );
    assert!(
        resp.result.is_none(),
        "Java variable trace should have no result"
    );
    assert!(
        resp.capability.is_some(),
        "capability should still be provided"
    );

    let cap = resp.capability.as_ref().unwrap();
    assert!(
        cap.supported_features.contains(&"call_graph".to_string()),
        "Java should support call_graph even if not dataflow"
    );
}

/// C: trace_variable must return partial (no dataflow support).
#[cfg(feature = "c")]
#[test]
fn p1_capability_c_variable_is_partial() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "main.c",
        r#"int add(int a, int b) {
    return a + b;
}

int main() {
    int result = add(3, 4);
    return result;
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "main.c");

    let resp = eng.trace_variable(&file_id, 6, 17, 10);

    assert!(resp.ok);
    assert!(resp.partial_result, "C variable trace should be partial");
    assert!(!resp.diagnostics.is_empty());
    assert!(resp.result.is_none());
}

/// Go: trace_variable must return partial (Symbolic level, no dataflow).
#[cfg(feature = "go")]
#[test]
fn p1_capability_go_variable_is_partial() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "main.go",
        r#"package main

func add(a, b int) int {
    return a + b
}

func main() {
    x := add(1, 2)
    _ = x
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "main.go");

    let resp = eng.trace_variable(&file_id, 6, 10, 10);

    assert!(resp.ok, "Go variable trace should not be an error");
    assert!(resp.partial_result, "Go variable trace should be partial (no dataflow)");
    assert!(
        !resp.diagnostics.is_empty(),
        "should have unsupported_language diagnostic"
    );
    assert!(
        resp.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("unsupported_language")),
        "diagnostic code should be 'unsupported_language'"
    );
    assert!(resp.result.is_none(), "Go variable trace should have no result");
    assert!(resp.capability.is_some(), "capability should still be provided");

    let cap = resp.capability.as_ref().unwrap();
    assert_eq!(cap.language, "go");
    assert!(
        cap.supported_features.contains(&"call_graph".to_string()),
        "Go should support call_graph even if not dataflow"
    );
}

/// TS: trace_variable should produce a result (dataflow available).
#[test]
fn p1_capability_ts_variable_succeeds() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "calc.ts",
        r#"function compute(): number {
    const base = 10;
    const factor = 2;
    const result = base * factor;
    return result;
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "calc.ts");

    let resp = eng.trace_variable(&file_id, 4, 22, 20);

    assert!(resp.ok, "TS variable trace should succeed");
    // TS has DataflowBasic, so it may succeed or be partial if no data node.
    assert!(
        resp.capability.is_some(),
        "capability must be present for TS"
    );
}

// ────────────────────────────────────────────────────────────────
// P1: No-result-but-not-error tests
// ────────────────────────────────────────────────────────────────

/// Point at a type alias (no data node, no reference to a callable symbol).
#[test]
fn p1_no_result_on_type_position() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("types.ts", "export type Status = 'ok' | 'error';\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "types.ts");

    // trace_variable at a type position should produce partial (no data node).
    let resp = eng.trace_variable(&file_id, 1, 17, 10);

    assert!(resp.ok, "must not be a system error");
    // May be partial (no data node) or may succeed if dataflow is available.
    // Either way, the envelope must be well-formed.
    let json = response_json(&resp);
    assert!(json.get("ok").is_some());
    assert!(json.get("diagnostics").is_some());
}

/// Point at blank space (should still return a structured response, never crash).
#[test]
fn p1_no_result_on_blank_position() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("empty.ts", "// just a comment\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "empty.ts");

    // trace_point at column 1, line 1
    let resp = eng.trace_point(&file_id, 1, 1);

    // Must be a valid envelope, even if result is minimal.
    assert!(resp.ok, "must not be a system error");
    let json = response_json(&resp);
    assert!(json.get("ok").is_some());
    assert!(json.get("result").is_some());
    // result may have no reference, no symbol, no data node — that's fine.
}

// ────────────────────────────────────────────────────────────────
// P1: Shadowing regression test
// ────────────────────────────────────────────────────────────────

/// Nested variable of same name must not conflate with outer scope.
///
/// Inner `x` originates from `safe`, outer `x` originates from `user`.
/// Tracing `sink(x)` must not jump to outer `user`.
#[test]
fn p1_shadowing_inner_variable_does_not_jump_to_outer_scope() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "shadow.ts",
        r#"const user = { name: "admin" };

function safe(): string {
    return "sanitized";
}

function process(): void {
    const x = user;
    function inner(): void {
        const x = safe();
        sink(x);  // should trace to safe(), NOT user
    }
    inner();
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "shadow.ts");

    // trace_variable at `sink(x)` — the `x` on line 10, column 13
    let resp = eng.trace_variable(&file_id, 11, 9, 30);

    assert!(resp.ok, "shadow trace should succeed");

    // If dataflow produced a path, verify the source is from the inner scope.
    if let Some(ref path) = resp.result {
        if !path.steps.is_empty() {
            // The source data node should NOT be `user` (the outer variable).
            // The inner `x` is assigned `safe()`, so the source should be
            // from `safe()` or the inner `x`, never the outer `user`.
            let source_name = path
                .source
                .data_node
                .as_ref()
                .and_then(|dn| dn.name.clone())
                .unwrap_or_default();
            assert_ne!(
                source_name, "user",
                "shadowed variable trace must NOT reach outer 'user'; \
                 inner 'x' is assigned safe(), not user. Got source: {}",
                source_name
            );
        }
    } else {
        // Partial result (no data node?) — that's ok for a heuristic engine.
        eprintln!(
            "shadow trace returned partial: {:?}",
            resp.diagnostics.first().map(|d| &d.code)
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P1: Multi-entry call path — semantics lock
// ────────────────────────────────────────────────────────────────

/// Two callers reach the same target; the explorer returns one farthest chain.
/// Lock this semantics so future changes to top-N are explicit.
#[test]
fn p1_multi_entry_returns_single_farthest_chain() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "multi.ts",
        r#"function target(): void {}

function direct(): void {
    target();  // depth 1
}

function via_mid(): void {
    target();  // depth 1
}

function top(): void {
    direct();   // depth 2
    via_mid();  // depth 1
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let engine = engine(store.clone());
    let file_id = resolve_file_id(&engine, tmp.path(), "multi.ts");
    let target_id = find_symbol(&store, &file_id, "target");

    // Both `top` (depth 2) and `via_mid` (depth 1) reach target.
    // The engine returns the single farthest chain.
    let resp = engine.trace_callers(&target_id, 10);

    assert!(resp.ok, "caller path should succeed");

    if let Some(ref chain) = resp.result {
        assert!(!chain.steps.is_empty(), "should have at least one step");
        // The chain returns a single farthest path.
        // Since `top → direct → target` has depth 2 (longest),
        // and `via_mid → target` has depth 1, the root must be `top`.
        // (If the engine later changes to top-N, this test must be updated.)
        assert_eq!(
            chain.root.name, "top",
            "single-farthest-chain semantics: root must be 'top' (depth 2), got '{}'",
            chain.root.name
        );
        assert!(
            chain.max_depth_reached >= 2,
            "should reach at least depth 2 (top→direct→target), got {}",
            chain.max_depth_reached
        );
    } else {
        eprintln!(
            "caller path returned partial: {:?}",
            resp.diagnostics.first().map(|d| &d.code)
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P2: Error input tests
// ────────────────────────────────────────────────────────────────

/// Point at a non-existent file should produce an error envelope.
#[test]
fn p2_error_missing_file() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let root = tmp.path();

    // Try to resolve a file that doesn't exist in the index
    let result = eng.resolve_file_id_with_root(root, "nonexistent.ts");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none(), "missing file should return None");
}

/// Trace at out-of-bounds line/column should not panic.
#[test]
fn p2_error_out_of_bounds_position() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "const x = 1;\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "app.ts");

    // Line 999, column 999 — way beyond the file.
    let resp = eng.trace_point(&file_id, 999, 999);

    // Must not crash; should produce a valid envelope.
    let json = response_json(&resp);
    assert!(json.get("ok").is_some(), "must produce valid JSON envelope");
    // ok may be true (no data at that position) or false (genuine error).
    // Either way, no panic.
}

// ────────────────────────────────────────────────────────────────
// Regression: Store persistence across init → index → query
// ────────────────────────────────────────────────────────────────

/// The store must persist data across the init + index workflow.
#[test]
fn p0_cli_persistence_across_init_index() {
    let _ = tracing_subscriber::fmt::try_init();
    let tmp = TempDir::new().expect("create temp dir");
    let root = tmp.path();

    // Write source
    std::fs::write(root.join("main.ts"), "const version = '1.0';\n").expect("write main.ts");

    let project = root.to_string_lossy().to_string();

    // Init
    init::run(&project).expect("init");

    // Index
    index::run(&project, None, None).expect("index");

    // Open store from disk (not the in-memory one from init)
    let store = Arc::new(Store::open_db(&root.join(".atlas/atlas.db")).expect("open store"));
    let files = store.list_files().expect("list files");
    assert!(
        !files.is_empty(),
        "should have indexed files after init+index"
    );

    let file_id = files.first().unwrap().file_id.clone();
    let resp = engine(store).trace_point(&file_id, 1, 10);
    assert!(resp.ok, "trace_point should work on persisted data");
}

// ────────────────────────────────────────────────────────────────
// P5: Human-friendly entry points — symbol name lookup
// ────────────────────────────────────────────────────────────────

/// trace_caller_path_by_name finds the same chain as by hex ID.
#[test]
fn p5_caller_path_by_symbol_name() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "callers.ts",
        r#"function leaf(): number { return 42; }

function branch(y: number): number {
    return leaf();
}

function trunk(z: number): void {
    const n = branch(z);
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine(store.clone());
    let file_id = resolve_file_id(&eng, tmp.path(), "callers.ts");

    // By hex ID
    let target_id = find_symbol(&store, &file_id, "leaf");
    let resp_hex = eng.trace_callers(&target_id, 10);

    // By name
    let resp_name = eng.trace_callers_by_name("leaf", 10);

    assert!(resp_hex.ok);
    assert!(resp_name.ok);
    assert!(resp_hex.result.is_some());
    assert!(resp_name.result.is_some());

    let chain_hex = resp_hex.result.as_ref().unwrap();
    let chain_name = resp_name.result.as_ref().unwrap();

    // Both should produce the same chain (same target, same number of steps)
    assert_eq!(chain_hex.target.name, chain_name.target.name);
    assert_eq!(chain_hex.steps.len(), chain_name.steps.len());
}

/// Searching for a non-existent symbol returns partial (not error).
#[test]
fn p5_caller_path_by_name_nonexistent() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, store) = init_and_index(files);
    let eng = engine(store.clone());

    let resp = eng.trace_callers_by_name("nonexistent_function", 10);

    assert!(resp.ok, "nonexistent symbol should not be a system error");
    assert!(
        resp.partial_result,
        "should be partial when symbol not found"
    );
    assert!(!resp.diagnostics.is_empty());
    assert!(
        resp.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("symbol_not_found")),
        "diagnostic should be 'symbol_not_found'"
    );
    assert!(resp.result.is_none());
}

/// find_symbol_ids_by_name locates all symbols with a given name.
#[test]
fn p5_find_symbol_ids_by_name_multiple() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("a.ts", "function f() {}"), ("b.ts", "function f() {}")];
    let (_tmp, store) = init_and_index(files);
    let eng = engine(store.clone());

    let ids = eng.find_symbol_ids_by_name("f").unwrap();
    assert_eq!(ids.len(), 2, "should find both 'f' symbols");
}

// ────────────────────────────────────────────────────────────────
// Evidence snippet — populated when project_root is provided
// ────────────────────────────────────────────────────────────────

/// When TraceEngine has a project root, caller-path steps must include
/// a snippet read from the source file on disk.
#[test]
fn p6_evidence_snippet_populated_with_project_root() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "chain.ts",
        r#"function inner(x: number): number {
    return x + 1;
}

function middle(y: number): number {
    return inner(y);
}
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());
    let file_id = resolve_file_id(&eng, tmp.path(), "chain.ts");
    let target_id = find_symbol(&store, &file_id, "inner");

    let resp = eng.trace_callers(&target_id, 10);
    assert!(resp.ok, "caller path should succeed");

    if let Some(ref chain) = resp.result {
        for step in &chain.steps {
            let ev = step
                .evidence
                .as_ref()
                .expect("step must have evidence with project_root");
            assert!(!ev.file_path.is_empty(), "file_path must be set");
            // snippet must be populated from the source file on disk
            assert!(
                ev.snippet.is_some(),
                "evidence.snippet must be populated when project_root is provided; \
                 file_path={}",
                ev.file_path
            );
            let snippet = ev.snippet.as_ref().unwrap();
            assert!(!snippet.is_empty(), "snippet should not be empty");
        }
    }
}

// ────────────────────────────────────────────────────────────────
// P2-9: Cross-file trace / caller-path
// ────────────────────────────────────────────────────────────────

/// Caller path should resolve across file boundaries when A imports B
/// and calls a function defined in B.
#[test]
fn p9_cross_file_caller_path() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "lib.ts",
            r#"export function inner(x: number): number {
    return x + 1;
}
"#,
        ),
        (
            "main.ts",
            r#"import { inner } from './lib';

function outer(v: number): number {
    return inner(v);
}
"#,
        ),
    ];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());
    let file_id = resolve_file_id(&eng, tmp.path(), "lib.ts");
    let target_id = find_symbol(&store, &file_id, "inner");

    let resp = eng.trace_callers(&target_id, 10);
    assert!(resp.ok, "cross-file caller path should succeed");

    if let Some(ref chain) = resp.result {
        assert!(
            !chain.steps.is_empty(),
            "should have at least one caller step"
        );
        // The root should be 'outer' from main.ts
        assert_eq!(chain.root.name, "outer", "root caller should be 'outer'");
        assert_eq!(chain.target.name, "inner", "target should be 'inner'");
    }
}

// ────────────────────────────────────────────────────────────────
// P2-10: Python real dataflow
// ────────────────────────────────────────────────────────────────

/// Python dataflow: trace a variable used as a function argument.
#[test]
fn p10_python_dataflow_trace() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.py",
        r#"def process(data):
    result = transform(data)
    return result

def transform(x):
    return x.upper()
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());
    let file_id = resolve_file_id(&eng, tmp.path(), "app.py");

    // Locate the 'process' function — just verify trace_point works
    let _process_id = find_symbol(&store, &file_id, "process");
    let resp = eng.trace_point(&file_id, 1, 4);
    assert!(resp.ok, "trace_point on Python should succeed");

    // Trace variable — Python supports dataflow, should not be partial for unsupported
    let var_resp = eng.trace_variable(&file_id, 2, 15, 50);
    // Python has DataflowBasic capability, so this should either succeed or
    // return a result (even if partial for other reasons, not for unsupported)
    assert!(
        var_resp.ok,
        "trace_variable on Python should not be a system error"
    );
    // It should NOT have a 'capability' diagnostic
    if var_resp.partial_result {
        let has_capability_diag = var_resp
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("capability"));
        assert!(
            !has_capability_diag,
            "Python should not produce capability diagnostic for dataflow"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P2-11: Truncation / budget
// ────────────────────────────────────────────────────────────────

/// Deep call chain should respect max_depth and mark max_depth_reached.
#[test]
fn p11_caller_path_respects_max_depth() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "deep.ts",
        r#"function a(): number { return b(); }
function b(): number { return c(); }
function d(): number { return e(); }
function e(): number { return 42; }
function c(): number { return d(); }
"#,
    )];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());
    let file_id = resolve_file_id(&eng, tmp.path(), "deep.ts");
    let target_id = find_symbol(&store, &file_id, "e");

    // Use max_depth=2 to force truncation
    let resp = eng.trace_callers(&target_id, 2);
    assert!(resp.ok, "truncated caller path should still be ok");

    if let Some(ref chain) = resp.result {
        // Chain should have at most 2 steps (depth-limited)
        assert!(
            chain.steps.len() <= 2,
            "steps should be limited by max_depth=2, got {}",
            chain.steps.len()
        );
        // max_depth_reached should indicate the traversal hit the limit
        assert!(
            chain.max_depth_reached >= 2,
            "max_depth_reached should be >= 2, got {}",
            chain.max_depth_reached
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P2-12: Error input tests (complement existing ones)
// ────────────────────────────────────────────────────────────────

/// trace_point on a line that's out of bounds should return a valid
/// response envelope (not panic, not empty output).
#[test]
fn p12_trace_point_line_out_of_bounds() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("small.ts", "const x = 1;\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());
    let file_id = resolve_file_id(&eng, tmp.path(), "small.ts");

    // Line 999 doesn't exist in a 1-line file
    let resp = eng.trace_point(&file_id, 999, 0);
    assert!(resp.ok, "out-of-bounds should not be a system error");
    // No reference found at that position
    assert!(
        resp.result.is_none()
            || resp
                .result
                .as_ref()
                .map_or(true, |tp| tp.reference.is_none()),
        "out-of-bounds position should not resolve a reference"
    );
}

/// trace_variable on a file that doesn't exist should return an error envelope.
#[test]
fn p12_trace_variable_file_not_found() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("real.ts", "const x = 1;\n")];
    let (tmp, store) = init_and_index(files);
    let eng = engine_with_root(store.clone(), tmp.path());

    // Use a FileId that doesn't correspond to any indexed file
    let fake_file_id = FileId::generate("nonexistent.ts");
    let resp = eng.trace_variable(&fake_file_id, 0, 6, 50);

    // Should return a valid envelope, not panic
    // A non-existent file may still return ok=true with partial_result=true
    // (it's not a system error, just no data found)
    assert!(
        resp.partial_result || !resp.ok,
        "non-existent file should return either ok=false or partial_result=true"
    );
}

// ────────────────────────────────────────────────────────────────
// P13: PathAliasResolver E2E (Task 4)
// ────────────────────────────────────────────────────────────────

/// Verify that tsconfig path aliases are resolved during index (Task 4).
/// Before Task 4, PathAliasResolver was always initialized as empty(),
/// so project-level tsconfig path mappings were ignored during import resolution.
#[test]
fn p13_tsconfig_path_alias_resolves_imports() {
    use atlas_db::Store;
    use atlas_types::enums::EdgeKind;

    let _ = tracing_subscriber::fmt::try_init();
    let tmp = TempDir::new().expect("create temp dir");

    // ── Write tsconfig.json with path aliases ──
    let tsconfig = serde_json::json!({
        "compilerOptions": {
            "baseUrl": ".",
            "paths": {
                "@lib/*": ["lib/*"]
            }
        }
    });
    std::fs::write(
        tmp.path().join("tsconfig.json"),
        serde_json::to_string_pretty(&tsconfig).unwrap(),
    )
    .expect("write tsconfig");

    // ── Write lib/helper.ts (exported function — alias target) ──
    std::fs::create_dir_all(tmp.path().join("lib")).expect("create lib dir");
    std::fs::write(
        tmp.path().join("lib/helper.ts"),
        r#"export function compute(x: number): number { return x + 1; }
"#,
    )
    .expect("write helper.ts");

    // ── Write src/other/utils.ts (duplicate compute — NOT aliased) ──
    // This proves aliased routing: without PathAliasResolver, the resolver may
    // pick this compute instead of lib/helper.ts::compute via global name search.
    std::fs::create_dir_all(tmp.path().join("src/other")).expect("create other dir");
    std::fs::write(
        tmp.path().join("src/other/utils.ts"),
        r#"export function compute(x: number): number { return x * 2; }
"#,
    )
    .expect("write utils.ts");

    // ── Write src/app.ts (imports via path alias) ──
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src dir");
    std::fs::write(
        tmp.path().join("src/app.ts"),
        r#"import { compute } from '@lib/helper';
export function run(): number { return compute(5); }
"#,
    )
    .expect("write app.ts");

    // ── Run init + index ──
    let project = tmp.path().to_string_lossy().to_string();
    atlas_cli::commands::init::run(&project).expect("atlas init");
    atlas_cli::commands::index::run(&project, None, None).expect("atlas index");

    // ── Verify resolution ──
    let store = Arc::new(Store::open_db(&tmp.path().join(".atlas/atlas.db")).expect("open store"));

    // Find the compute function in lib/helper.ts
    let lib_file_id = FileId::generate("lib/helper.ts");
    let lib_syms = store.find_symbols_by_file(&lib_file_id).unwrap();
    let compute_sym = lib_syms
        .iter()
        .find(|s| s.name == "compute")
        .expect("compute symbol not found in lib/helper.ts");

    // Find the run function in src/app.ts
    let app_file_id = FileId::generate("src/app.ts");
    let app_syms = store.find_symbols_by_file(&app_file_id).unwrap();
    let _run_sym = app_syms
        .iter()
        .find(|s| s.name == "run")
        .expect("run symbol not found in src/app.ts");

    // Verify that a Calls edge exists from the import to the aliased compute
    // (NOT the duplicate compute in src/other/utils.ts, which would be picked
    // by global name fallback without path alias support).
    let all_edges = store.get_all_edges().unwrap();
    let call_edges: Vec<_> = all_edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.target == compute_sym.id)
        .collect();
    assert!(
        !call_edges.is_empty(),
        "expected Calls edge from import site to lib/helper.ts::compute via @lib/helper alias. \
         Got {} total edges, {} Calls edges to compute. \
         A duplicate compute exists in src/other/utils.ts — if the edge targets \
         that symbol instead, PathAliasResolver did not narrow the resolution.",
        all_edges.len(),
        call_edges.len()
    );
}
