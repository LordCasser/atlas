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

use atlas_cli::commands::{index, init};
use atlas_engine::ids::{FileId, SymbolId};
use atlas_engine::ContextBuilder;
use atlas_engine::GraphEngine;
use atlas_engine::SearchEngine;
use atlas_engine::Store;
use atlas_mcp::tools::ToolRouter;
use serde_json::{json, Value};
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
    init::run(&project).expect("init");
    index::run(&project, None, &[], "structural").expect("index");

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
        .find(|f| f.path == rel_path || f.path.ends_with(&format!("/{}", rel_path)))
        .expect(&format!("file not found: {}", rel_path));
    file.file_id
}

/// Find a symbol by name within a file.
fn find_symbol(store: &Store, file_id: &FileId, name: &str) -> SymbolId {
    let syms = store.find_symbols_by_file(file_id).expect("find symbols");
    syms.iter()
        .find(|s| s.name == name)
        .expect(&format!("symbol '{}' not found", name))
        .id
}

/// Call a tool and return the parsed content JSON plus is_error.
fn call_tool(router: &ToolRouter, name: &str, args: Value) -> (Value, bool) {
    let result = router.call_tool(name, &args);
    // Parse the first content block as JSON
    let text = match result.content.first() {
        Some(atlas_mcp::protocol::ContentBlock::Text { text }) => text.clone(),
        None => String::new(),
    };
    let content_json: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            println!("[DEBUG] JSON parse error: {:?}", e);
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
        tool_names.contains(&"trace_point"),
        "tools/list must include atlas_trace_point"
    );
    assert!(
        tool_names.contains(&"trace_variable"),
        "tools/list must include atlas_trace_variable"
    );
    assert!(
        tool_names.contains(&"trace_caller_path"),
        "tools/list must include atlas_trace_caller_path"
    );

    // Verify trace_point schema has the right properties
    let trace_point = list
        .tools
        .iter()
        .find(|t| t.name == "trace_point")
        .expect("trace_point tool");
    let props = trace_point
        .input_schema
        .properties
        .as_ref()
        .expect("trace_point must have inputSchema.properties");
    assert!(
        props.get("file_path").is_some(),
        "schema must have file_path"
    );
    assert!(props.get("line").is_some(), "schema must have line");
    assert!(props.get("column").is_some(), "schema must have column");

    // Verify trace_caller_path schema has symbol property
    let caller_path = list
        .tools
        .iter()
        .find(|t| t.name == "trace_caller_path")
        .expect("trace_caller_path tool");
    let cp_props = caller_path
        .input_schema
        .properties
        .as_ref()
        .expect("trace_caller_path must have inputSchema.properties");
    assert!(cp_props.get("symbol").is_some(), "schema must have symbol");
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "src/app.ts");

    let args = json!({
        "file_id": file_id.to_hex(),
        "line": 6,
        "column": 20,
    });
    let (content_json, is_error) = call_tool(&router, "trace_point", args);

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
    let (_tmp, router) = build_router(files);

    let args = json!({});
    let (content_json, is_error) = call_tool(&router, "trace_point", args);

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
    let (_tmp, router) = build_router(files);

    let args = json!({
        "file_path": "src/calc.ts",
        "line": 1,
        "column": 10,
    });
    let (content_json, is_error) = call_tool(&router, "trace_point", args);

    assert!(!is_error, "file_path-based trace_point must succeed");
    assert_envelope_fields(&content_json);
    assert!(content_json
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "calc.ts");

    let args = json!({
        "file_id": file_id.to_hex(),
        "line": 4,
        "column": 22,
        "max_depth": 20,
    });
    let (content_json, is_error) = call_tool(&router, "trace_variable", args);

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
    let (_tmp, router) = build_router(files);

    let args = json!({});
    let (content_json, is_error) = call_tool(&router, "trace_variable", args);

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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "chain.ts");

    let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
    let inner = syms
        .iter()
        .find(|s| s.name == "inner")
        .expect("inner symbol not found");
    let symbol_hex = inner.id.to_hex();

    let args = json!({ "symbol": symbol_hex, "max_depth": 10 });
    let (content_json, is_error) = call_tool(&router, "trace_caller_path", args);

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
    let (_tmp, router) = build_router(files);

    let args = json!({ "symbol": "not-a-valid-hex-id", "max_depth": 10 });
    let (content_json, is_error) = call_tool(&router, "trace_caller_path", args);

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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "root.ts");

    let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
    let standalone = syms
        .iter()
        .find(|s| s.name == "standalone")
        .expect("standalone symbol not found");
    let symbol_hex = standalone.id.to_hex();

    let args = json!({ "symbol": symbol_hex, "max_depth": 10 });
    let (content_json, is_error) = call_tool(&router, "trace_caller_path", args);

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
    let (_tmp, router) = build_router(files);

    let (_content_json, is_error) = call_tool(&router, "nonexistent_tool", json!({}));
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
    let (_tmp, router) = build_router(files);
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
            "trace_point",
            json!({ "file_id": file_id.to_hex(), "line": 2, "column": 30 }),
        ),
        (
            "trace_variable",
            json!({ "file_id": file_id.to_hex(), "line": 2, "column": 30, "max_depth": 10 }),
        ),
        (
            "trace_caller_path",
            json!({ "symbol": symbol_hex, "max_depth": 10 }),
        ),
    ];

    for (tool_name, args) in &trace_cases {
        let (content_json, _is_error) = call_tool(&router, tool_name, args.clone());
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "fn.ts");

    let args = json!({ "file_id": file_id.to_hex(), "line": 1, "column": 10 });
    let (content_json, is_error) = call_tool(&router, "trace_variable", args);

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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");

    let args = json!({ "file_id": file_id.to_hex(), "line": 1, "column": 10 });

    for tool_name in &[
        "trace_point",
        "trace_variable",
        "trace_caller_path",
    ] {
        let tool_args = if *tool_name == "trace_caller_path" {
            let syms = store.find_symbols_by_file(&file_id).expect("find symbols");
            if let Some(sym) = syms.first() {
                json!({ "symbol": sym.id.to_hex(), "max_depth": 5 })
            } else {
                continue;
            }
        } else {
            args.clone()
        };

        let result = router.call_tool(tool_name, &tool_args);
        assert!(
            !result.content.is_empty(),
            "{} must return at least one content block",
            tool_name
        );
    }
}

// ────────────────────────────────────────────────────────────────
// P1: Capability boundary — Java trace_variable is DataflowBasic
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "App.java");

    let args = json!({ "file_id": file_id.to_hex(), "line": 3, "column": 17 });
    let (content_json, is_error) = call_tool(&router, "trace_variable", args);

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
        "Java DataflowBasic trace must not be gated as unsupported_language"
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
        "dataflow_basic",
        "Java should advertise DataflowBasic"
    );
}

// ────────────────────────────────────────────────────────────────
// P1: Capability boundary — Go trace_variable is DataflowBasic
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "main.go");

    let args = json!({ "file_id": file_id.to_hex(), "line": 6, "column": 8 });
    let (content_json, is_error) = call_tool(&router, "trace_variable", args);

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
        "dataflow_basic",
        "Go should advertise DataflowBasic"
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");
    let target_id = find_symbol(&store, &file_id, "target").to_hex();

    // Call by hex
    let (json_hex, is_err_hex) = call_tool(
        &router,
        "trace_caller_path",
        json!({
            "symbol": target_id,
        }),
    );
    assert!(!is_err_hex, "trace_caller_path by hex must succeed");

    // Call by name
    let (json_name, is_err_name) = call_tool(
        &router,
        "trace_caller_path",
        json!({
            "symbol_name": "target",
        }),
    );
    assert!(!is_err_name, "trace_caller_path by name must succeed");

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
    let (_tmp, router) = build_router(files);

    let (json, is_error) = call_tool(
        &router,
        "trace_caller_path",
        json!({
            "symbol_name": "ghost_function",
        }),
    );

    assert!(!is_error, "nonexistent symbol must not be a system error");
    assert!(
        json.get("partial_result")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "should be partial_result=true"
    );
    assert!(json.get("result").is_some(), "result field must be present");
    assert!(json.get("result").unwrap().is_null(), "result must be null");
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
    let (_tmp, router) = build_router(files);
    let store = router.store();
    let file_id = find_file_id(&store, _tmp.path(), "lib.ts");
    let target_id = find_symbol(&store, &file_id, "inner");

    let (json, is_error) = call_tool(
        &router,
        "trace_caller_path",
        json!({
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
    let (_tmp, router) = build_router(files);
    let store = router.store();
    let _file_id = find_file_id(&store, _tmp.path(), "app.py");

    let (json, is_error) = call_tool(
        &router,
        "trace_variable",
        json!({
            "file_path": "app.py",
            "line": 2,
            "column": 15,
        }),
    );

    assert!(!is_error, "Python trace_variable should not be error");
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
    let (_tmp, router) = build_router(files);
    let store = router.store();
    let file_id = find_file_id(&store, _tmp.path(), "deep.ts");
    let target_id = find_symbol(&store, &file_id, "e");

    let (json, is_error) = call_tool(
        &router,
        "trace_caller_path",
        json!({
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
                "steps should be limited by max_depth=2, got {}",
                steps
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
    let (_tmp, router) = build_router(files);

    let (json, is_error) = call_tool(
        &router,
        "trace_point",
        json!({
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
    let router = ToolRouter::new(store, search, context, tmp.path().to_path_buf());

    // atlas_callgraph with a valid pre-built graph should succeed
    let (json, is_error) = call_tool(
        &router,
        "callgraph",
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
    // callgraph response has the standard tool result fields (not error envelope)
    assert_eq!(
        json.get("symbol").and_then(|v| v.as_str()),
        Some("f"),
        "response must have symbol field"
    );
    assert!(
        json.get("nodes_found").and_then(|v| v.as_u64()).is_some(),
        "response must have nodes_found field"
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
    let (_tmp, router) = build_router(files);

    // Trace from `c` on the console.log line — BFS will hit max_depth=1 quickly
    let (json, is_error) = call_tool(
        &router,
        "trace_variable",
        json!({
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
            "truncated trace should include max_depth_truncated diagnostic, got: {:?}",
            diags
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
    let (_tmp, router) = build_router(files);
    let store = router.store();
    let file_id = find_file_id(&store, _tmp.path(), "deep.ts");
    let target_id = find_symbol(&store, &file_id, "d");

    let (json, is_error) = call_tool(
        &router,
        "trace_caller_path",
        json!({
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
        "truncated caller path should include max_depth_truncated diagnostic, got: {:?}",
        diags
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");
    let greet_id = find_symbol(&store, &file_id, "greet");

    let (json, is_error) = call_tool(&router, "usages", json!({ "symbol": greet_id.to_hex() }));
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
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");

    let (json, is_error) = call_tool(
        &router,
        "dependencies",
        json!({ "file_id": file_id.to_hex() }),
    );
    assert!(!is_error, "dependencies should succeed");
    let deps = json.get("dependencies").and_then(|d| d.as_array());
    assert!(deps.is_some(), "should have dependencies array");
    assert!(
        deps.unwrap().len() > 0,
        "should have at least one dependency"
    );
}

#[test]
fn mcp_usages_empty_for_unreferenced() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("app.ts", "function unused(): void {}\n// never called\n")];
    let (_tmp, router) = build_router(files);
    let store = Store::open_db(&_tmp.path().join(".atlas/atlas.db")).expect("open store");
    let file_id = find_file_id(&store, _tmp.path(), "app.ts");
    let sym_id = find_symbol(&store, &file_id, "unused");

    let (json, is_error) = call_tool(&router, "usages", json!({ "symbol": sym_id.to_hex() }));
    assert!(
        !is_error,
        "usages should succeed even for unused symbols"
    );
    let total = json
        .get("total_usages")
        .and_then(|v| v.as_u64())
        .unwrap_or(999);
    assert_eq!(total, 0, "unused function should have 0 usages");
}
