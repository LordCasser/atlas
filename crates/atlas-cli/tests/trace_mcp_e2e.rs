//! MCP end-to-end tests for `atlas trace` tools.
//!
//! These tests exercise the MCP ToolRouter (the core JSON-RPC dispatch) with
//! real indexed projects, verifying that:
//!
//! - tools/list registers all trace tools with correct schemas
//! - tools/call produces valid CallToolResult (JSON with isError, content)
//! - trace responses adhere to the TraceQueryResponse contract
//! - error cases (invalid params, unknown tools) produce structured errors
//! - `isError` correctly maps from `ok` in the response envelope
//!
//! Requires the `mcp` feature.
//!
//! Run: `cargo test --test trace_mcp_e2e --features mcp`
//!      `cargo test --test trace_mcp_e2e --features all-languages,mcp`

// The entire test suite is feature-gated on `mcp`.
#![cfg(feature = "mcp")]

use atlas_cli::commands::index;
use atlas_cli::runtime::{CommandContext, DbMode};
use atlas_engine::ContextBuilder;
use atlas_engine::GraphEngine;
use atlas_engine::SearchEngine;
use atlas_engine::Store;
use atlas_engine::ids::{FileId, SymbolId};
use atlas_mcp::tools::ToolRouter;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

/// Create a temp project with the given sources, init + index, return the
/// project root dir and a fully constructed ToolRouter.
fn build_router(files: &[(&str, &str)]) -> (TempDir, ToolRouter) {
    let tmp = TempDir::new().expect("create temp dir");

    for (rel_path, content) in files {
        let file_path = tmp.path().join(rel_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&file_path, content).expect("write source file");
    }

    let project = tmp.path().to_string_lossy().to_string();
    CommandContext::open(&project, DbMode::InitOrCreate).expect("atlas init");
    index::run(&project, &[], &[], &[], "structural").expect("index");

    let store = Arc::new(Store::open_db(&tmp.path().join(".atlas/atlas.db")).expect("open store"));
    let graph = Arc::new(GraphEngine::from_store(&store, 0.3).expect("graph engine"));

    let search = SearchEngine::new(store.clone(), graph.clone());
    let context = ContextBuilder::new(store.clone(), graph.clone());

    let router = ToolRouter::new(store, search, context, tmp.path().to_path_buf());
    (tmp, router)
}

/// Find a FileId by relative path within the project.
fn find_file_id(store: &Store, _root: &Path, rel_path: &str) -> FileId {
    let files = store.list_files().expect("list files");
    let file = files
        .iter()
        .find(|f| f.path == rel_path || f.path.ends_with(&format!("/{rel_path}")))
        .unwrap_or_else(|| panic!("file not found: {rel_path}"));
    file.file_id
}

/// Find a symbol by name within a file.
fn find_symbol(store: &Store, file_id: &FileId, name: &str) -> SymbolId {
    let syms = store.find_symbols_by_file(file_id).expect("find symbols");
    syms.iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("symbol '{name}' not found"))
        .id
}

/// Call a tool and return the parsed content JSON plus is_error.
/// Mirrors the MCP server's call_tool flow: ensures graph init for
/// graph-backed tools before dispatching.
fn call_tool(router: &mut ToolRouter, name: &str, args: Value) -> (Value, bool) {
    if ToolRouter::tool_requires_graph(name) {
        if let Err(e) = router.ensure_graph_initialized() {
            let err = json!({ "ok": false, "error": format!("{:#}", e) });
            return (err, true);
        }
    }
    let result = router.call_tool(name, &args);
    // Parse the first content block as JSON
    let text = match result.content.first() {
        Some(atlas_mcp::protocol::ContentBlock::Text { text }) => text.clone(),
        None => String::new(),
    };
    let content_json: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            println!("[DEBUG] JSON parse error: {e:?}");
            println!(
                "[DEBUG] Raw text (first 500 chars): {:?}",
                &text[..text.len().min(500)]
            );
            json!({ "raw": text })
        }
    };
    (content_json, result.is_error.unwrap_or(false))
}

/// Assert that a CallToolResult JSON has all 6 envelope fields.
fn assert_envelope_fields(json: &Value) {
    let fields = [
        "ok",
        "kind",
        "capability",
        "partial_result",
        "diagnostics",
        "result",
    ];
    for field in &fields {
        assert!(
            json.get(field).is_some(),
            "envelope JSON must contain field '{}'. Got keys: {:?}",
            field,
            json.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P0: tools/list — trace tools registered
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_tools_list_includes_trace_tools() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, router) = build_router(files);

    let list = router.list_tools();
    let tool_names: Vec<&str> = list.tools.iter().map(|t| t.name.as_str()).collect();

    assert!(
        tool_names.contains(&"trace"),
        "tools/list must include trace (unified trace tool: kind=point/variable/forward/callers)"
    );

    // Verify trace schema has kind (required for dispatch) and position params
    let trace_tool = list
        .tools
        .iter()
        .find(|t| t.name == "trace")
        .expect("trace tool");
    let props = trace_tool
        .input_schema
        .properties
        .as_ref()
        .expect("trace must have inputSchema.properties");
    assert!(props.get("kind").is_some(), "schema must have kind");
    assert!(
        props.get("file_path").is_some(),
        "schema must have file_path"
    );
    assert!(props.get("line").is_some(), "schema must have line");
    assert!(props.get("column").is_some(), "schema must have column");
    assert!(props.get("symbol").is_some(), "schema must have symbol");
}

// ────────────────────────────────────────────────────────────────
// P0: tools/call — trace_point
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_trace_point_valid_params_returns_result() {
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
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "src/app.ts");

    let args = json!({
        "kind": "point",
        "file_id": file_id.to_hex(),
        "line": 6,
        "column": 20,
    });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(!is_error, "valid trace_point call must not set isError");
    assert_envelope_fields(&content_json);
    assert!(
        content_json
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "trace_point response.ok must be true"
    );
    assert_eq!(
        content_json
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "trace_point"
    );
    assert!(
        content_json.get("result").is_some(),
        "result field must exist"
    );
}

#[test]
fn p0_mcp_trace_point_missing_params_returns_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let args = json!({"kind": "point"});
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(is_error, "missing params must set isError=true");
    // Must return a TraceQueryResponse envelope with ok=false
    assert_eq!(
        content_json.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "envelope ok must be false for missing params"
    );
    assert_envelope_fields(&content_json);
    // Diagnostic should mention the missing parameter
    let diags = content_json
        .get("diagnostics")
        .and_then(|v| v.as_array())
        .unwrap();
    assert!(!diags.is_empty(), "should have at least one diagnostic");
}

#[test]
fn p0_mcp_trace_point_with_file_path_resolves() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "src/calc.ts",
        r#"function add(a: number, b: number): number {
    return a + b;
}
"#,
    )];
    let (_tmp, mut router) = build_router(files);

    let args = json!({
        "kind": "point",
        "file_path": "src/calc.ts",
        "line": 1,
        "column": 10,
    });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(!is_error, "file_path-based trace_point must succeed");
    assert_envelope_fields(&content_json);
    assert!(
        content_json
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    );
}

// ────────────────────────────────────────────────────────────────
// P0: tools/call — trace_variable
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_trace_variable_returns_dataflow_result() {
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
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "calc.ts");

    let args = json!({
        "kind": "variable",
        "file_id": file_id.to_hex(),
        "line": 4,
        "column": 22,
        "max_depth": 20,
    });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(
        !is_error,
        "trace_variable must not set isError for valid input"
    );
    assert_envelope_fields(&content_json);
    assert_eq!(
        content_json
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "trace_variable"
    );
    assert!(content_json.get("result").is_some());
}

#[test]
fn p0_mcp_trace_variable_missing_params_returns_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let args = json!({"kind": "variable"});
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(is_error, "missing file_id/file_path must set isError");
    // Must return a TraceQueryResponse envelope with ok=false
    assert_eq!(
        content_json.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "envelope ok must be false for missing params"
    );
    assert_envelope_fields(&content_json);
}

// ────────────────────────────────────────────────────────────────
// P0: tools/call — trace_caller_path
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_trace_caller_path_returns_chain() {
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
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "chain.ts");

    let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
    let inner = syms
        .iter()
        .find(|s| s.name == "inner")
        .expect("inner symbol not found");
    let symbol_hex = inner.id.to_hex();

    let args = json!({ "kind": "callers", "symbol": symbol_hex, "max_depth": 10 });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(
        !is_error,
        "caller_path must not set isError for valid input"
    );
    assert_envelope_fields(&content_json);
    assert_eq!(
        content_json
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "trace_callers"
    );

    let result = content_json.get("result").expect("result must exist");
    assert!(!result.is_null(), "result must not be null");
    let steps = result.get("steps").and_then(|s| s.as_array());
    assert!(steps.is_some(), "caller chain must have steps field");
}

#[test]
fn p0_mcp_trace_caller_path_invalid_symbol_returns_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let args = json!({ "kind": "callers", "symbol": "not-a-valid-hex-id", "max_depth": 10 });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(is_error, "invalid symbol hex must set isError");
    // Must return a TraceQueryResponse envelope with ok=false
    assert_eq!(
        content_json.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "envelope ok must be false for invalid symbol"
    );
    assert_envelope_fields(&content_json);
}

#[test]
fn p0_mcp_trace_caller_path_root_function_returns_partial_not_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("root.ts", "function standalone(): number { return 42; }\n")];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "root.ts");

    let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
    let standalone = syms
        .iter()
        .find(|s| s.name == "standalone")
        .expect("standalone symbol not found");
    let symbol_hex = standalone.id.to_hex();

    let args = json!({ "kind": "callers", "symbol": symbol_hex, "max_depth": 10 });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(
        !is_error,
        "root function with no callers must NOT set isError"
    );
    assert_envelope_fields(&content_json);
    assert!(
        content_json
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "response.ok must be true"
    );
    assert!(
        content_json
            .get("partial_result")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "no-callers must be partial_result=true"
    );
}

// ────────────────────────────────────────────────────────────────
// P0: Error handling — unknown tool
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_unknown_tool_returns_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let (_content_json, is_error) = call_tool(&mut router, "nonexistent_tool", json!({}));
    assert!(is_error, "unknown tool must set isError=true");
}

// ────────────────────────────────────────────────────────────────
// P0: JSON contract — all six envelope fields present
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_all_trace_tools_return_full_envelope_json() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "code.ts",
        r#"function helper(x: number): number { return x * 2; }
function main(): void { const r = helper(21); }
main();
"#,
    )];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "code.ts");

    let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
    let helper = syms
        .iter()
        .find(|s| s.name == "helper")
        .expect("helper symbol not found");
    let symbol_hex = helper.id.to_hex();

    let trace_cases: Vec<(&str, Value)> = vec![
        (
            "trace",
            json!({ "kind": "point", "file_id": file_id.to_hex(), "line": 2, "column": 30 }),
        ),
        (
            "trace",
            json!({ "kind": "variable", "file_id": file_id.to_hex(), "line": 2, "column": 30, "max_depth": 10 }),
        ),
        (
            "trace",
            json!({ "kind": "callers", "symbol": symbol_hex, "max_depth": 10 }),
        ),
    ];

    for (tool_name, args) in &trace_cases {
        let (content_json, _is_error) = call_tool(&mut router, tool_name, args.clone());
        assert_envelope_fields(&content_json);
        eprintln!(
            "{} envelope: ok={}, kind={}",
            tool_name,
            content_json
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            content_json
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P0: isError mapping — partial results must NOT set isError
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_mcp_partial_result_not_is_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("fn.ts", "declare function foo(): void;\n")];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "fn.ts");

    let args = json!({ "kind": "variable", "file_id": file_id.to_hex(), "line": 1, "column": 10 });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(!is_error, "partial result must NOT set isError=true");
    assert_envelope_fields(&content_json);
    assert!(
        content_json
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "partial result must have ok=true"
    );
}

// ────────────────────────────────────────────────────────────────
// P2: Bounded output — truncation safety
// ────────────────────────────────────────────────────────────────

#[test]
fn p2_mcp_output_truncation_safety() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const VERSION = '1.0.0';\n")];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");

    let args = json!({ "kind": "point", "file_id": file_id.to_hex(), "line": 1, "column": 10 });

    // trace(point)
    let result = router.call_tool("trace", &args);
    assert!(
        !result.content.is_empty(),
        "trace(point) must return content"
    );

    // trace(variable)
    let var_args =
        json!({ "kind": "variable", "file_id": file_id.to_hex(), "line": 1, "column": 10 });
    let result = router.call_tool("trace", &var_args);
    assert!(
        !result.content.is_empty(),
        "trace(variable) must return content"
    );

    // trace(callers)
    let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
    if let Some(sym) = syms.first() {
        let caller_args = json!({ "kind": "callers", "symbol": sym.id.to_hex(), "max_depth": 5 });
        let result = router.call_tool("trace", &caller_args);
        assert!(
            !result.content.is_empty(),
            "trace(callers) must return content"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P1: Capability boundary — Java trace_variable is DataflowFull
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "java")]
#[test]
fn p1_mcp_java_trace_variable_is_partial() {
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
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "App.java");

    let args = json!({ "kind": "variable", "file_id": file_id.to_hex(), "line": 3, "column": 17 });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(!is_error, "Java variable trace must not be an error");
    let diags = content_json
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .expect("must have diagnostics array");
    let has_unsupported = diags
        .iter()
        .any(|d| d.get("code").and_then(|c| c.as_str()) == Some("unsupported_language"));
    assert!(
        !has_unsupported,
        "Java DataflowFull trace must not be gated as unsupported_language"
    );
    assert!(
        content_json.get("capability").is_some(),
        "Java capability must be present"
    );
    let cap = content_json.get("capability").unwrap();
    assert_eq!(
        cap.get("language").and_then(|v| v.as_str()).unwrap_or(""),
        "java",
        "capability language must be 'java'"
    );
    assert_eq!(
        cap.get("capability_level")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "dataflow_full",
        "Java should advertise DataflowFull"
    );
}

// ────────────────────────────────────────────────────────────────
// P1: Capability boundary — Go trace_variable is DataflowFull
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "go")]
#[test]
fn p1_mcp_go_trace_variable_is_partial() {
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
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "main.go");

    let args = json!({ "kind": "variable", "file_id": file_id.to_hex(), "line": 6, "column": 8 });
    let (content_json, is_error) = call_tool(&mut router, "trace", args);

    assert!(!is_error, "Go variable trace must not be an error");
    let diags = content_json
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .expect("must have diagnostics array");
    let has_unsupported = diags
        .iter()
        .any(|d| d.get("code").and_then(|c| c.as_str()) == Some("unsupported_language"));
    assert!(
        !has_unsupported,
        "Go DataflowBasic trace must not be gated as unsupported_language"
    );
    assert!(
        content_json.get("capability").is_some(),
        "Go capability must be present"
    );
    let cap = content_json.get("capability").unwrap();
    assert_eq!(
        cap.get("language").and_then(|v| v.as_str()).unwrap_or(""),
        "go",
        "capability language must be 'go'"
    );
    assert_eq!(
        cap.get("capability_level")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "dataflow_full",
        "Go should advertise DataflowFull"
    );
}

// ────────────────────────────────────────────────────────────────
// P5: MCP caller path by symbol_name (human-friendly entry point)
// ────────────────────────────────────────────────────────────────

#[test]
fn p5_mcp_caller_path_by_symbol_name() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.ts",
        r#"function target(): void {}

function caller(): void {
    target();
}
"#,
    )];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");
    let target_id = find_symbol(&store, &file_id, "target").to_hex();

    // Call by hex
    let (json_hex, is_err_hex) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "callers",
            "symbol": target_id,
        }),
    );
    assert!(!is_err_hex, "trace(callers) by hex must succeed");

    // Call by name
    let (json_name, is_err_name) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "callers",
            "symbol": "target",
        }),
    );
    assert!(!is_err_name, "trace(callers) by name must succeed");

    // Both should produce the same result
    assert_eq!(
        json_hex
            .get("result")
            .and_then(|r| r.get("target"))
            .and_then(|t| t.get("name")),
        json_name
            .get("result")
            .and_then(|r| r.get("target"))
            .and_then(|t| t.get("name")),
    );
    assert_eq!(
        json_hex
            .get("result")
            .and_then(|r| r.get("steps"))
            .and_then(|s| s.as_array().map(|a| a.len())),
        json_name
            .get("result")
            .and_then(|r| r.get("steps"))
            .and_then(|s| s.as_array().map(|a| a.len())),
    );
}

/// trace_caller_path with symbol_name for non-existent symbol returns partial.
#[test]
fn p5_mcp_caller_path_by_name_nonexistent() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "callers",
            "symbol": "ghost_function",
        }),
    );

    assert!(is_error, "nonexistent symbol must produce an error");
    // Must return a TraceQueryResponse envelope with ok=false
    assert_eq!(
        json.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "envelope ok must be false for nonexistent symbol"
    );
}

// ────────────────────────────────────────────────────────────────
// P2: Additional coverage
// ────────────────────────────────────────────────────────────────

/// Cross-file caller-path: import from one file, call in another.
#[test]
fn p9_mcp_cross_file_caller_path() {
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
    let (_tmp, mut router) = build_router(files);
    let store = router.store();
    let file_id = find_file_id(&store, _tmp.path(), "lib.ts");
    let target_id = find_symbol(&store, &file_id, "inner");

    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "callers",
            "symbol": target_id.to_hex(),
        }),
    );

    assert!(!is_error, "cross-file caller path should not be error");
    assert!(
        json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        "ok must be true"
    );

    if let Some(result) = json.get("result") {
        if !result.is_null() {
            let target_name = result
                .get("target")
                .and_then(|t| t.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            assert_eq!(target_name, "inner", "target should be 'inner'");
        }
    }
}

/// Python dataflow via MCP: trace_variable should work for Python.
#[test]
fn p10_mcp_python_dataflow() {
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
    let (_tmp, mut router) = build_router(files);
    let store = router.store();
    let _file_id = find_file_id(&store, _tmp.path(), "app.py");

    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "variable",
            "file_path": "app.py",
            "line": 2,
            "column": 15,
        }),
    );

    assert!(!is_error, "Python trace(variable) should not be error");
    // Python should not produce a capability diagnostic
    if let Some(diagnostics) = json.get("diagnostics").and_then(|d| d.as_array()) {
        let has_cap_diag = diagnostics
            .iter()
            .any(|d| d.get("code").and_then(|c| c.as_str()) == Some("capability"));
        assert!(
            !has_cap_diag,
            "Python should not produce capability diagnostic"
        );
    }
}

/// Deep call chain with max_depth truncation.
#[test]
fn p11_mcp_caller_path_max_depth() {
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
    let (_tmp, mut router) = build_router(files);
    let store = router.store();
    let file_id = find_file_id(&store, _tmp.path(), "deep.ts");
    let target_id = find_symbol(&store, &file_id, "e");

    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "callers",
            "symbol": target_id.to_hex(),
            "max_depth": 2,
        }),
    );

    assert!(!is_error, "truncated caller path should not be error");
    if let Some(result) = json.get("result") {
        if !result.is_null() {
            let steps = result
                .get("steps")
                .and_then(|s| s.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            assert!(
                steps <= 2,
                "steps should be limited by max_depth=2, got {steps}"
            );
            let max_depth_reached = result
                .get("max_depth_reached")
                .and_then(|m| m.as_u64())
                .unwrap_or(0);
            assert!(max_depth_reached >= 2, "max_depth_reached should be >= 2");
        }
    }
}

/// Out-of-bounds line via MCP: should return a valid envelope, not crash.
#[test]
fn p12_mcp_trace_point_out_of_bounds() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("small.ts", "const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "point",
            "file_path": "small.ts",
            "line": 999,
            "column": 0,
        }),
    );

    // Out-of-bounds should not be a system error — just no data found
    assert!(!is_error, "out-of-bounds should not be system error");
    // The response should be a valid envelope
    assert!(json.get("ok").is_some(), "envelope must have 'ok' field");
    assert!(
        json.get("kind").is_some(),
        "envelope must have 'kind' field"
    );
    assert!(
        json.get("partial_result").is_some(),
        "envelope must have 'partial_result' field"
    );
    assert!(
        json.get("diagnostics").is_some(),
        "envelope must have 'diagnostics' field"
    );
}

// ────────────────────────────────────────────────────────────────
// P12a: Graph error handling (Task 3 — no panic on graph failure)
// ────────────────────────────────────────────────────────────────

/// Verify that graph-related tools return results when graph is valid
/// (built once at startup from DB snapshot, never rebuilt per-request).
#[test]
fn p12a_mcp_graph_error_returns_structured_response() {
    let _ = tracing_subscriber::fmt::try_init();

    // Build a minimal project with one file
    let (tmp, _router) = build_router(&[("app.ts", "function f() {}\n")]);

    let store = Arc::new(Store::open_db(&tmp.path().join(".atlas/atlas.db")).expect("open store"));

    let graph = Arc::new(GraphEngine::from_store(&store, 0.3).expect("graph engine"));
    let search = SearchEngine::new(store.clone(), graph.clone());
    let context = ContextBuilder::new(store.clone(), graph.clone());

    // Graph is built once at startup; engine holds the valid snapshot.
    // Per-request rebuild was removed — graph queries use the static snapshot.
    let mut router = ToolRouter::new(store, search, context, tmp.path().to_path_buf());

    // atlas_callgraph with a valid pre-built graph should succeed
    let (json, is_error) = call_tool(
        &mut router,
        "calls",
        json!({
            "symbol": "f",
            "depth": 2,
        }),
    );

    // With pre-built graph, graph operations should succeed
    assert!(
        !is_error,
        "graph operation should succeed with pre-built snapshot"
    );
    // calls response has the standard tool result fields (not error envelope)
    assert_eq!(
        json.get("symbol").and_then(|v| v.as_str()),
        Some("f"),
        "response must have symbol field"
    );
    assert!(
        json.get("total_nodes_visited")
            .and_then(|v| v.as_u64())
            .is_some(),
        "response must have total_nodes_visited field"
    );
}

// ────────────────────────────────────────────────────────────────
// P7: Truncation diagnostics (Item 7 — trace output polish)
// ────────────────────────────────────────────────────────────────

/// Variable trace with max_depth=1 forces truncation — verify diagnostics.
#[test]
fn p7a_mcp_trace_variable_truncation_diagnostic() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "chain.ts",
        r#"function foo(): void {
    const a = 1;
    const b = a + 2;
    const c = b * 3;
    console.log(c);
}
"#,
    )];
    let (_tmp, mut router) = build_router(files);

    // Trace from `c` on the console.log line — BFS will hit max_depth=1 quickly
    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "variable",
            "file_path": "chain.ts",
            "line": 5,
            "column": 15,
            "max_depth": 1,
        }),
    );

    // Must not crash — either partial with truncation diagnostic or empty
    assert!(!is_error, "truncated trace should not error");

    // Check that diagnostics mentions truncation if partial_result is true
    if json
        .get("partial_result")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let diags = json
            .get("diagnostics")
            .and_then(|d| d.as_array())
            .map(|a| a.to_vec())
            .unwrap_or_default();
        let has_truncation = diags
            .iter()
            .any(|d| d.get("code").and_then(|c| c.as_str()) == Some("max_depth_truncated"));
        assert!(
            has_truncation,
            "truncated trace should include max_depth_truncated diagnostic, got: {diags:?}"
        );
    }
}

/// Caller path with max_depth=1 forces truncation — verify partial_result + diagnostic.
#[test]
fn p7b_mcp_caller_path_truncation_diagnostic() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "deep.ts",
        r#"function a(): number { return b(); }
function b(): number { return c(); }
function c(): number { return d(); }
function d(): number { return 42; }
"#,
    )];
    let (_tmp, mut router) = build_router(files);
    let store = router.store();
    let file_id = find_file_id(&store, _tmp.path(), "deep.ts");
    let target_id = find_symbol(&store, &file_id, "d");

    let (json, is_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "callers",
            "symbol": target_id.to_hex(),
            "max_depth": 1,
        }),
    );

    // Must not crash — partial with truncation diagnostic
    assert!(!is_error, "truncated caller path should not error");
    assert_eq!(
        json.get("partial_result").and_then(|v| v.as_bool()),
        Some(true),
        "max_depth=1 with 3-level chain should set partial_result=true"
    );

    let diags = json
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .map(|a| a.to_vec())
        .unwrap_or_default();
    let has_truncation = diags
        .iter()
        .any(|d| d.get("code").and_then(|c| c.as_str()) == Some("max_depth_truncated"));
    assert!(
        has_truncation,
        "truncated caller path should include max_depth_truncated diagnostic, got: {diags:?}"
    );
}

// ────────────────────────────────────────────────────────────────
// MCP tool coverage: usages, dependencies, dependents
// ────────────────────────────────────────────────────────────────

#[test]
fn mcp_usages_returns_references() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.ts",
        r#"function greet(name: string): string { return "Hello, " + name; }
greet("World");
"#,
    )];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");
    let greet_id = find_symbol(&store, &file_id, "greet");

    let (json, is_error) = call_tool(
        &mut router,
        "symbol",
        json!({ "view": "usages", "qname": greet_id.to_hex() }),
    );
    assert!(!is_error, "usages should succeed");
    assert!(json.get("usages").is_some(), "should have usages array");
}

#[test]
fn mcp_dependencies_returns_imports() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        ("lib.ts", "export const VERSION = '1.0';"),
        (
            "app.ts",
            "import { VERSION } from './lib';\nconsole.log(VERSION);",
        ),
    ];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let _file_id = find_file_id(&store, _tmp.path(), "app.ts");

    let (json, is_error) = call_tool(
        &mut router,
        "file_dependencies",
        json!({ "direction": "outgoing", "file_path": "app.ts" }),
    );
    assert!(!is_error, "dependencies should succeed");
    let deps = json.get("dependencies").and_then(|d| d.as_array());
    assert!(deps.is_some(), "should have dependencies array");
    assert!(
        !deps.unwrap().is_empty(),
        "should have at least one dependency"
    );
}

#[test]
fn mcp_usages_empty_for_unreferenced() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "function unused(): void {}\n// never called\n")];
    let (_tmp, mut router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");
    let sym_id = find_symbol(&store, &file_id, "unused");

    let (json, is_error) = call_tool(
        &mut router,
        "symbol",
        json!({ "view": "usages", "qname": sym_id.to_hex() }),
    );
    assert!(!is_error, "usages should succeed even for unused symbols");
    let total = json
        .get("total_usages")
        .and_then(|v| v.as_u64())
        .unwrap_or(999);
    assert_eq!(total, 0, "unused function should have 0 usages");
}

// ────────────────────────────────────────────────────────────────
// open_project tests
// ────────────────────────────────────────────────────────────────

#[test]
fn open_project_in_tools_list() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, router) = build_router(files);

    let list = router.list_tools();
    let tool_names: Vec<&str> = list.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        tool_names.contains(&"project"),
        "tools/list must include project (action='open'/'status'/'files')"
    );

    // Verify project tool has action parameter and project_path for open
    let tool = list
        .tools
        .iter()
        .find(|t| t.name == "project")
        .expect("project tool");
    let required = tool.input_schema.required.as_ref();
    // 'project' has no top-level required params (action determines requirements)
    assert!(
        required.is_none() || required.unwrap().is_empty(),
        "project tool should have no hard-required params at top level"
    );
    let props = tool
        .input_schema
        .properties
        .as_ref()
        .expect("project must have properties");
    assert!(
        props.get("project_path").is_some(),
        "project schema must expose project_path"
    );
    assert!(
        props.get("scan_files").is_some(),
        "project schema must expose scan_files"
    );
    assert!(
        props.get("background").is_some(),
        "project schema must expose background"
    );
    assert!(
        props.get("action").is_some(),
        "project schema must expose action"
    );
    assert!(
        props.get("index").is_none(),
        "project tool must not expose indexing parameters"
    );
    assert!(
        props.get("analysis").is_none(),
        "project tool must not expose indexing parameters"
    );
    assert!(
        props.get("include").is_none(),
        "project tool must not expose indexing parameters"
    );
    assert!(
        props.get("exclude").is_none(),
        "project tool must not expose indexing parameters"
    );
}

#[test]
fn index_schema_supports_background_and_include() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, router) = build_router(files);

    let list = router.list_tools();
    let tool = list
        .tools
        .iter()
        .find(|t| t.name == "index")
        .expect("index tool");
    let props = tool
        .input_schema
        .properties
        .as_ref()
        .expect("index must have properties");
    assert!(
        props.get("include").is_some(),
        "index schema must expose include"
    );
    assert!(
        props.get("background").is_some(),
        "index schema must expose background"
    );
    assert!(
        props.get("analysis").is_none(),
        "index schema must not expose analysis; MCP index is always manifest"
    );
}

#[test]
fn index_rejects_analysis_parameter() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let (json, is_error) = call_tool(&mut router, "index", json!({ "analysis": "structural" }));

    assert!(is_error, "index must reject analysis parameter");
    let errors = json["errors"].as_array().expect("errors array");
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap_or("").contains("analysis")),
        "error should mention unsupported analysis: {json:?}"
    );
}

#[test]
fn open_project_background_activates_on_wait_for_task() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let target = TempDir::new().expect("target temp dir");
    std::fs::write(target.path().join("other.ts"), "export const y = 2;\n")
        .expect("write target file");
    let expected = target
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let result = router.call_tool(
        "project",
        &json!({
            "action": "open",
            "project_path": target.path().to_string_lossy(),
            "storage": "memory",
            "background": true
        }),
    );
    assert_eq!(result.is_error, Some(false));
    let text = match &result.content[0] {
        atlas_mcp::protocol::ContentBlock::Text { text } => text,
    };
    let started: Value = serde_json::from_str(text).expect("background response json");
    let task_id = started["task_id"].as_str().expect("task_id").to_string();

    let wait = router.call_tool(
        "wait_for_task",
        &json!({
            "task_id": task_id,
            "timeout_secs": 5,
            "poll_interval_secs": 1
        }),
    );
    assert_eq!(wait.is_error, Some(false));
    let wait_text = match &wait.content[0] {
        atlas_mcp::protocol::ContentBlock::Text { text } => text,
    };
    let completed: Value = serde_json::from_str(wait_text).expect("wait response json");
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["activation"], "activated");
    assert_eq!(completed["activated_project"], expected);

    let status = router.call_tool("project", &json!({"action": "status"}));
    let status_text = match &status.content[0] {
        atlas_mcp::protocol::ContentBlock::Text { text } => text,
    };
    let status_json: Value = serde_json::from_str(status_text).expect("status json");
    assert_eq!(status_json["project"]["active_project"], expected);
    assert_eq!(status_json["project"]["storage"], "memory");
}

#[test]
fn index_background_completes_via_wait_for_task() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let started = router.call_tool(
        "index",
        &json!({
            "background": true
        }),
    );
    assert_eq!(started.is_error, Some(false));
    let started_text = match &started.content[0] {
        atlas_mcp::protocol::ContentBlock::Text { text } => text,
    };
    let started_json: Value = serde_json::from_str(started_text).expect("index task json");
    let task_id = started_json["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();

    let wait = router.call_tool(
        "wait_for_task",
        &json!({
            "task_id": task_id,
            "timeout_secs": 5,
            "poll_interval_secs": 1
        }),
    );
    assert_eq!(wait.is_error, Some(false));
    let wait_text = match &wait.content[0] {
        atlas_mcp::protocol::ContentBlock::Text { text } => text,
    };
    let completed: Value = serde_json::from_str(wait_text).expect("index wait json");
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["result"]["ok"], true);
    assert!(
        completed["result"]["files_indexed"].as_u64().unwrap_or(0) >= 1,
        "background index should report indexed files"
    );
}

#[test]
fn open_project_missing_project_path_returns_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let (json, is_error) = call_tool(&mut router, "project", json!({"action": "open"}));
    assert!(
        is_error,
        "project(action=open) without project_path must return is_error=true"
    );

    let err_msg = json.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err_msg.contains("project_path"),
        "error should mention missing project_path"
    );
}

#[test]
fn open_project_nonexistent_path_returns_error() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "export const x = 1;\n")];
    let (_tmp, mut router) = build_router(files);

    let (json, is_error) = call_tool(
        &mut router,
        "project",
        json!({ "action": "open", "project_path": "/nonexistent/path/12345" }),
    );
    assert!(
        is_error,
        "project(action=open) with nonexistent path must return is_error=true"
    );
    assert!(!json["ok"].as_bool().unwrap_or(true), "ok must be false");
}

#[test]
fn open_project_memory_no_index_switches_project() {
    let _ = tracing_subscriber::fmt::try_init();

    // Create a fresh project with source files
    let tmp = TempDir::new().expect("temp dir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.ts"),
        "export function greet() { return 'hello'; }\n",
    )
    .unwrap();

    // Start router with initial project (dummy)
    let files = &[("dummy.ts", "export const x = 1;\n")];
    let (_tmp_initial, mut router) = build_router(files);

    // Open the fresh project without indexing
    let (json, is_error) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
            "storage": "memory",
        }),
    );
    assert!(
        !is_error,
        "project(action=open) (memory) should succeed: {json:?}"
    );
    assert!(json["ok"].as_bool().unwrap_or(false), "ok must be true");

    // Status should reflect the new project
    let (status_json, status_error) =
        call_tool(&mut router, "project", json!({"action": "status"}));
    assert!(!status_error, "status should succeed");
    assert_eq!(
        status_json["project"]["storage"].as_str().unwrap_or(""),
        "memory",
        "storage should be memory"
    );
    let active = status_json["project"]["active_project"]
        .as_str()
        .unwrap_or("");
    assert!(
        active.contains(&tmp.path().display().to_string()),
        "active_project should point to new project"
    );
}

#[test]
fn open_project_then_index_enables_search() {
    let _ = tracing_subscriber::fmt::try_init();

    // Create a fresh project with source files
    let tmp = TempDir::new().expect("temp dir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.ts"),
        "export function greet(name: string): string {\n  return `Hello, ${name}`;\n}\n",
    )
    .unwrap();

    // Start router with initial dummy project
    let files = &[("dummy.ts", "export const x = 1;\n")];
    let (_tmp_initial, mut router) = build_router(files);

    // Open the fresh project, then index the active project.
    let (json, is_error) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
            "storage": "memory",
        }),
    );
    assert!(
        !is_error,
        "project(action=open) (memory) should succeed: {json:?}"
    );
    assert!(json["ok"].as_bool().unwrap_or(false), "ok must be true");

    let (index, index_error) = call_tool(&mut router, "index", json!({}));
    assert!(!index_error, "index should succeed: {index:?}");

    // Index result should show discovered files
    assert!(
        index["files_discovered"].as_u64().unwrap_or(0) >= 1,
        "should discover at least 1 file, got: {index:?}"
    );

    // Status should show indexed files
    let (status_json, status_error) =
        call_tool(&mut router, "project", json!({"action": "status"}));
    assert!(!status_error, "status should succeed");
    assert!(
        status_json["summary"]["files"].as_i64().unwrap_or(0) >= 1,
        "status should show indexed files"
    );

    // Search should find the greet function
    let (search_json, search_error) = call_tool(
        &mut router,
        "search",
        json!({ "query": "greet", "scope": "src" }),
    );
    assert!(!search_error, "search should succeed");
    let results = search_json["results"]
        .as_array()
        .expect("search should have results array");
    assert!(
        !results.is_empty(),
        "search for 'greet' should find results"
    );

    // trace_variable should work with lazy dataflow
    // First get the file_id from files tool
    let (files_json, _) = call_tool(&mut router, "project", json!({"action": "files"}));
    let file_list = files_json["files"]
        .as_array()
        .expect("files should be array");
    let _greet_file = file_list
        .iter()
        .find(|f| f["path"].as_str().is_some_and(|p| p.contains("lib.ts")))
        .expect("should find lib.ts");

    // trace_point should resolve the greet function
    let (trace_json, trace_error) = call_tool(
        &mut router,
        "trace",
        json!({
            "kind": "point",
            "file_path": "src/lib.ts",
            "line": 1,
            "column": 17,
        }),
    );
    assert!(!trace_error, "trace(point) should succeed: {trace_json:?}");
}

#[test]
fn mcp_search_requires_scope() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("src/lib.ts", "export function greet() { return 'hi'; }\n")];
    let (_tmp, mut router) = build_router(files);

    // With a manual full index (build_router runs `atlas index structural`),
    // scope is no longer required — search defaults to "." (entire project).
    let (search_json, search_error) = call_tool(&mut router, "search", json!({ "query": "greet" }));

    assert!(
        !search_error,
        "search without scope should succeed with manual index: {search_json:?}"
    );
    // Scope defaults to project root; response is a valid ScopedSearchResponse.
    assert_eq!(search_json["query"].as_str(), Some("greet"));
    assert_eq!(search_json["parse_level"].as_str(), Some("structural"));
    assert_eq!(search_json["precise"].as_bool(), Some(true));
}

#[test]
fn mcp_search_large_scope_stays_manifest_level() {
    let _ = tracing_subscriber::fmt::try_init();
    let tmp = TempDir::new().expect("temp dir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..12 {
        let name = if i == 7 { "target_fn" } else { "helper_fn" };
        std::fs::write(
            src.join(format!("file{i}.ts")),
            format!("export function {name}_{i}() {{ return {i}; }}\n"),
        )
        .unwrap();
    }

    let files = &[("dummy.ts", "export const x = 1;\n")];
    let (_tmp_initial, mut router) = build_router(files);
    let (_, open_error) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
            "storage": "memory",
        }),
    );
    assert!(!open_error, "project(action=open) should succeed");
    let (_, index_error) = call_tool(&mut router, "index", json!({}));
    assert!(!index_error, "index should succeed");

    let (search_json, search_error) = call_tool(
        &mut router,
        "search",
        json!({ "query": "target_fn_7", "scope": "src", "kind": "function" }),
    );

    assert!(!search_error, "search should succeed: {search_json:?}");
    // Search no longer turns medium/full-project scopes into implicit
    // structural indexing. It should answer from manifest facts and guide the
    // caller to narrow scope for deeper parsing.
    assert_eq!(search_json["parse_level"].as_str(), Some("manifest"));
    assert_eq!(search_json["precise"].as_bool(), Some(false));
    let results = search_json["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|r| r["name"].as_str() == Some("target_fn_7")),
        "manifest search should find exact function without synchronous structural parsing: {search_json:?}"
    );
}

#[test]
fn open_project_rejects_indexing_parameters() {
    let _ = tracing_subscriber::fmt::try_init();
    let tmp = TempDir::new().expect("temp dir");
    std::fs::write(tmp.path().join("lib.ts"), "export const x = 1;\n").unwrap();
    let files = &[("dummy.ts", "export const x = 1;\n")];
    let (_tmp_initial, mut router) = build_router(files);

    let (json, is_error) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
            "index": true,
        }),
    );

    assert!(is_error, "project(action=open) must reject index=true");
    assert!(
        json["error"].as_str().unwrap_or("").contains("index tool"),
        "error should point users to index: {json:?}"
    );
}

#[test]
fn open_project_persistent_creates_db_and_survives_reopen() {
    let _ = tracing_subscriber::fmt::try_init();

    let tmp = TempDir::new().expect("temp dir");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("util.ts"),
        "export function add(a: number, b: number): number { return a + b; }\n",
    )
    .unwrap();

    // Start router with initial dummy project
    let files = &[("dummy.ts", "export const x = 1;\n")];
    let (_tmp_initial, mut router) = build_router(files);

    // First: open persistent storage, then index the active project.
    let (json1, is_error1) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
            "storage": "persistent",
        }),
    );
    assert!(
        !is_error1,
        "first project(action=open) should succeed: {json1:?}"
    );
    assert!(json1["ok"].as_bool().unwrap_or(false), "ok must be true");

    let (index_json, index_error) = call_tool(&mut router, "index", json!({}));
    assert!(!index_error, "index should succeed: {index_json:?}");

    // Verify .atlas/atlas.db exists
    let db_path = tmp.path().join(".atlas/atlas.db");
    assert!(
        db_path.exists(),
        ".atlas/atlas.db should exist after persistent project(action=open)"
    );

    // Status should show persistent
    let (status_json, _) = call_tool(&mut router, "project", json!({"action": "status"}));
    assert_eq!(
        status_json["project"]["storage"].as_str().unwrap_or(""),
        "persistent",
        "storage should be persistent"
    );
    assert!(
        status_json["summary"]["files"].as_i64().unwrap_or(0) >= 1,
        "status should show indexed files"
    );

    // Search should find the add function
    let (search_json, _) = call_tool(
        &mut router,
        "search",
        json!({ "query": "add", "scope": "src" }),
    );
    let results = search_json["results"].as_array().unwrap();
    assert!(!results.is_empty(), "search for 'add' should find results");

    // Re-open the same project without a storage override. The default auto
    // mode should reuse the existing persistent DB without re-indexing.
    let (json2, is_error2) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
        }),
    );
    assert!(!is_error2, "re-open should succeed: {json2:?}");
    assert_eq!(
        json2["storage"].as_str(),
        Some("persistent"),
        "auto storage should reuse existing .atlas/atlas.db"
    );
    assert!(
        json2["suggestion"]
            .as_str()
            .is_some_and(|s| s.contains("Reusable persistent index detected via project status")),
        "response should explain why persistent storage was selected: {json2:?}"
    );

    // Search should still find add after reopen without re-index
    let (search_json2, _) = call_tool(
        &mut router,
        "search",
        json!({ "query": "add", "scope": "src" }),
    );
    let results2 = search_json2["results"].as_array().unwrap();
    assert!(
        !results2.is_empty(),
        "search after reopen should still find 'add'"
    );
}

#[test]
fn open_project_auto_ignores_empty_persistent_db() {
    let _ = tracing_subscriber::fmt::try_init();

    let tmp = TempDir::new().expect("temp dir");
    std::fs::write(tmp.path().join("lib.ts"), "export const x = 1;\n").unwrap();

    let files = &[("dummy.ts", "export const seed = 1;\n")];
    let (_tmp_initial, mut router) = build_router(files);

    // Explicit persistent open creates a schema but no reusable index.
    let (persistent_json, persistent_error) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
            "storage": "persistent",
        }),
    );
    assert!(
        !persistent_error,
        "explicit persistent open should succeed: {persistent_json:?}"
    );
    assert_eq!(persistent_json["storage"].as_str(), Some("persistent"));

    // Auto should read project status from the candidate DB, see index mode
    // "none", and avoid treating the empty DB as a usable persistent index.
    let (auto_json, auto_error) = call_tool(
        &mut router,
        "project",
        json!({
            "action": "open",
            "project_path": tmp.path().to_string_lossy(),
        }),
    );
    assert!(!auto_error, "auto open should succeed: {auto_json:?}");
    assert_eq!(
        auto_json["storage"].as_str(),
        Some("memory"),
        "auto storage must not reuse a persistent DB whose status has no index"
    );
    assert_eq!(
        auto_json["db_path"].as_str(),
        Some(":memory:"),
        "auto fallback should be an in-memory store"
    );
}
