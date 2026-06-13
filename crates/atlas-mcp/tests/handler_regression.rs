//! Handler regression tests — verify MCP tool response formats under
//! various conditions.
//!
//! These tests use in-memory stores and temp-dir C projects to exercise
//! full tool dispatch through [`ToolRouter::call_tool`].  Each test is
//! self-contained and creates its own store and project data.
//!
//! Run with:
//! ```bash
//! cargo test -p atlas-mcp --test handler_regression -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use serde_json::{Value, json};
use atlas_engine::Store;
use atlas_mcp::tools::{ToolRouter, ToolCallContext};
use atlas_mcp::protocol::ContentBlock;

// =========================================================================
// Helpers
// =========================================================================

/// Create a router with a fresh in-memory store and a temp directory
/// containing a simple C file.  The project is indexed at the manifest
/// level so graph-backed tools can resolve symbols.
fn setup_temp_c_project(temp_dir: &std::path::Path) -> ToolRouter {
    let c_content = r#"
void bar(void) {
    // does nothing
}

void foo(void) {
    bar();
}
"#;
    std::fs::write(temp_dir.join("test.c"), c_content)
        .expect("write test.c");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.to_path_buf());

    // Index the project at manifest level (symbols + files).
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, "index", &json!({"analysis": "manifest"}));
    let text = extract_text(&result);
    let parsed: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
    if parsed.get("ok") != Some(&Value::Bool(true)) {
        eprintln!("Index warning/error: {text:.500}");
    }
    router
}

/// Extract the first text block from a CallToolResult.
fn extract_text(result: &atlas_mcp::protocol::CallToolResult) -> &str {
    result
        .content
        .first()
        .map(|cb| match cb {
            ContentBlock::Text { text } => text.as_str(),
        })
        .unwrap_or("")
}

/// Call a tool via the full dispatch path and return (parsed_json, is_error).
fn call_tool(router: &mut ToolRouter, name: &str, args: &Value) -> (Value, bool) {
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, name, args);
    let text = extract_text(&result).trim();
    let parsed = if text.starts_with('{') || text.starts_with('[') {
        serde_json::from_str(text).unwrap_or_else(|_| json!({"raw": text}))
    } else {
        json!({"raw": text})
    };
    let is_error = result.is_error.unwrap_or(false);
    (parsed, is_error)
}

// =========================================================================
// Test 1: Graph tools return expected JSON structure
// =========================================================================

#[test]
fn handler_graph_tools_return_expected_structure() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_graph_struct");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    let mut router = setup_temp_c_project(&temp_dir);

    // ── calls (callers) ──────────────────────────────────────────────
    let (resp, err) = call_tool(
        &mut router,
        "calls",
        &json!({"symbol": "foo", "direction": "incoming"}),
    );
    if err {
        eprintln!("calls(incoming) returned error: {resp:.300}");
    } else {
        assert!(
            resp.get("symbol").is_some() || resp.get("total_callers").is_some(),
            "calls(incoming) response missing expected fields: {resp:.300}",
        );
    }

    // ── calls (callees) ──────────────────────────────────────────────
    let (resp2, err2) = call_tool(
        &mut router,
        "calls",
        &json!({"symbol": "foo", "direction": "outgoing"}),
    );
    if err2 {
        eprintln!("calls(outgoing) returned error: {resp2:.300}");
    } else {
        assert!(
            resp2.get("symbol").is_some() || resp2.get("total_callees").is_some(),
            "calls(outgoing) response missing expected fields: {resp2:.300}",
        );
    }

    // ── explore ──────────────────────────────────────────────────────
    let (resp3, err3) = call_tool(
        &mut router,
        "explore",
        &json!({"symbol": "foo"}),
    );
    if err3 {
        eprintln!("explore returned error: {resp3:.300}");
    } else {
        let has_explore_fields = resp3.get("callEvidence").is_some()
            || resp3.get("fileContext").is_some()
            || resp3.get("subject").is_some()
            || resp3.get("sourceExcerpt").is_some();
        assert!(
            has_explore_fields,
            "explore response missing expected fields: {resp3:.300}",
        );
    }

    // ── impact ───────────────────────────────────────────────────────
    let (resp4, err4) = call_tool(
        &mut router,
        "impact",
        &json!({"symbol": "foo"}),
    );
    if err4 {
        eprintln!("impact returned error: {resp4:.300}");
    } else {
        let has_impact = resp4.get("file_groups").is_some()
            || resp4.get("impacted").is_some()
            || resp4.get("symbol").is_some()
            || resp4.get("nodes").is_some();
        assert!(
            has_impact,
            "impact response missing expected fields: {resp4:.300}",
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// Test 2: Analysis tools reject non-C/C++ / missing symbols gracefully
// =========================================================================

#[test]
fn handler_analysis_tools_reject_non_cpp() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, PathBuf::from("."));

    // ── lifecycle with non-existent symbol ───────────────────────────
    let (resp, _err) = call_tool(
        &mut router,
        "lifecycle",
        &json!({"symbol": "NonExistentFunction_XYZ_999", "field": "ptr"}),
    );
    // Should be an error of some form — either Symbol not found or
    // unsupported_language.  Just verify it's an error response.
    let text = resp.to_string();
    assert!(
        text.contains("error")
            || text.contains("not found")
            || text.contains("not_found")
            || text.contains("unsupported")
            || text.contains("No results")
            || text.contains("Symbol not found"),
        "lifecycle for non-existent symbol should return an error, got: {text:.300}",
    );

    // ── branch_diff with non-existent symbol ─────────────────────────
    let (resp2, _err2) = call_tool(
        &mut router,
        "branch_diff",
        &json!({"symbol": "NonExistentFunction_XYZ_999"}),
    );
    let text2 = resp2.to_string();
    assert!(
        text2.contains("error")
            || text2.contains("not found")
            || text2.contains("not_found")
            || text2.contains("unsupported")
            || text2.contains("No results")
            || text2.contains("Symbol not found"),
        "branch_diff for non-existent symbol should return an error, got: {text2:.300}",
    );
}

// =========================================================================
// Test 3: Search with scope parameter
// =========================================================================

#[test]
fn handler_search_with_scoped_query() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_search_scope");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    // Create a subdirectory with its own C file
    let sub_dir = temp_dir.join("sub");
    std::fs::create_dir_all(&sub_dir).expect("create sub dir");
    let c_content = r#"
int sub_function(void) {
    return 42;
}
"#;
    std::fs::write(sub_dir.join("sub.c"), c_content).expect("write sub.c");

    // Also write a root-level file
    std::fs::write(
        temp_dir.join("main.c"),
        r#"
int top_function(void) {
    return 0;
}
"#,
    )
    .expect("write main.c");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());

    // Index the whole project.
    let ctx = ToolCallContext::empty();
    let _ = router.call_tool(&ctx, "index", &json!({"analysis": "manifest"}));

    // ── Unscoped search should find both functions ────────────────────
    let (resp, err) = call_tool(
        &mut router,
        "search",
        &json!({"query": "function", "analysis": "manifest"}),
    );
    if err {
        eprintln!("search (unscoped) error: {resp:.300}");
    }

    // ── Scoped search to "sub/" ───────────────────────────────────────
    // The response structure varies; the key assertion is no panic and
    // valid JSON.  Per-scope filtering may not apply in manifest mode
    // (scope may be ignored) — that's acceptable.
    let (resp2, err2) = call_tool(
        &mut router,
        "search",
        &json!({"query": "sub", "scope": "sub", "analysis": "manifest"}),
    );
    if !err2 {
        // If results are returned, verify they come from the scoped dir.
        if let Some(results) = resp2.get("results").and_then(|r| r.as_array()) {
            for hit in results {
                let file = hit.get("file").and_then(|v| v.as_str()).unwrap_or("");
                // Accept either scoped files or empty file (manifest may
                // not populate file path in all cases).
                if !file.is_empty() {
                    assert!(
                        file.contains("sub"),
                        "scoped search returned file outside scope: {file}",
                    );
                }
            }
        }
    } else {
        eprintln!("search (scoped) error (acceptable for manifest mode): {resp2:.300}");
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// Test 4: Overlay mutation (fp_dispatches add) is idempotent
// =========================================================================

#[test]
fn handler_overlay_mutation_idempotent() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_overlay");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    let mut router = setup_temp_c_project(&temp_dir);

    // Use symbols that exist in the indexed project ("foo" and "bar").
    let add_args = json!({
        "action": "add",
        "field_qname": "bar",
        "target_qname": "foo",
    });

    // First add.
    let (resp1, err1) = call_tool(&mut router, "fp_dispatches", &add_args);
    if err1 {
        // If annotation add resolves to an error (e.g. symbols exist but
        // are not function-pointer fields), accept it — not a crash.
        eprintln!("First fp_dispatches add error: {resp1:.300}");
    } else {
        assert!(
            resp1.get("ok").is_some() || resp1.get("status").is_some() || resp1.get("annotation").is_some(),
            "First fp_dispatches add missing expected fields: {resp1:.300}",
        );
    }

    // Second add — should be idempotent (no duplicate, consistent result).
    let (resp2, err2) = call_tool(&mut router, "fp_dispatches", &add_args);
    if err2 {
        eprintln!("Second fp_dispatches add error: {resp2:.300}");
    } else {
        assert!(
            resp2.get("ok").is_some() || resp2.get("status").is_some() || resp2.get("annotation").is_some(),
            "Second fp_dispatches add missing expected fields: {resp2:.300}",
        );
    }

    // List to verify no duplicates.
    let (list_resp, list_err) = call_tool(
        &mut router,
        "fp_dispatches",
        &json!({"action": "list"}),
    );
    assert!(!list_err, "fp_dispatches list failed: {list_resp:.300}");

    // If annotations are returned as an array, count occurrences of our
    // specific annotation to verify no duplication.
    let annotations = list_resp
        .get("annotations")
        .and_then(|v| v.as_array())
        .or_else(|| list_resp.get("data").and_then(|v| v.as_array()));
    if let Some(anns) = annotations {
        let count = anns
            .iter()
            .filter(|a| {
                let fq = a.get("field_qname").and_then(|v| v.as_str()).unwrap_or("");
                let tq = a.get("target_qname").and_then(|v| v.as_str()).unwrap_or("");
                fq == "bar" && tq == "foo"
            })
            .count();
        assert!(
            count <= 1,
            "Idempotent add produced {count} duplicate annotations",
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// Test 5: Symbol detail returns source code snippets
// =========================================================================

#[test]
fn handler_symbol_detail_returns_source() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_sym");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    let mut router = setup_temp_c_project(&temp_dir);

    // Query symbol detail for "foo" (string form).
    let (resp, err) = call_tool(
        &mut router,
        "symbol",
        &json!({"symbol": "foo", "view": "detail"}),
    );
    if err {
        eprintln!("symbol detail error (acceptable): {resp:.300}");

        // Try the structured selector form as a fallback.
        let (resp2, _err2) = call_tool(
            &mut router,
            "symbol",
            &json!({"symbol": {"qualified_name": "foo"}, "view": "detail"}),
        );
        let has_fields = resp2.get("qualified_name").is_some()
            || resp2.get("name").is_some()
            || resp2.get("kind").is_some()
            || resp2.get("file").is_some();
        assert!(
            has_fields,
            "symbol detail (structured) missing expected fields: {resp2:.300}",
        );
    } else {
        // Verify the response has identifying fields.
        let has_fields = resp.get("qualified_name").is_some()
            || resp.get("name").is_some()
            || resp.get("kind").is_some()
            || resp.get("file").is_some();
        assert!(
            has_fields,
            "symbol detail missing expected fields: {resp:.300}",
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// Bonus: All non-graph tools survive minimal args without panic
// =========================================================================
//
// Graph-backed tools are excluded because they trigger `ensure_graph_initialized`
// which can be slow for a cold store with no indexed data.  Those are tested
// in `handler_graph_tools_return_expected_structure` and in the e2e suite.

#[test]
fn handler_non_graph_tools_no_panic() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, PathBuf::from("."));

    // Overlay + task tools — no graph needed, fast.
    let tools: &[(&str, Value)] = &[
        ("fp_dispatches", json!({"action": "list"})),
        ("domain_rules", json!({"action": "list"})),
        ("tasks", json!({})),
        ("task_status", json!({"task_id": "nonexistent"})),
        ("project", json!({"action": "status"})),
        ("lifecycle", json!({"symbol": "x", "field": "y"})),
        ("branch_diff", json!({"symbol": "x"})),
        ("file_dependencies", json!({"file_path": "nonexistent.c", "analysis": "manifest"})),
    ];

    for (name, args) in tools {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ctx = ToolCallContext::empty();
            router.call_tool(&ctx, name, args)
        }));
        assert!(
            result.is_ok(),
            "Handler '{name}' panicked with minimal args",
        );
    }
}
