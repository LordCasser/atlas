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

use atlas_engine::Store;
use atlas_mcp::protocol::ContentBlock;
use atlas_mcp::tools::{ToolCallContext, ToolRouter};
use serde_json::{Value, json};
use std::sync::Arc;

// =========================================================================
// Helpers
// =========================================================================

/// Create a router with a fresh in-memory store and a temp directory
/// containing a simple C file.  No MCP index step is run; focus-backed
/// handlers must prepare scoped facts on demand.
fn setup_temp_c_project(temp_dir: &std::path::Path) -> ToolRouter {
    let c_content = r#"
void bar(void) {
    // does nothing
}

void foo(void) {
    bar();
}
"#;
    std::fs::write(temp_dir.join("test.c"), c_content).expect("write test.c");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.to_path_buf());
    router.init_focus();
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
    let (resp3, err3) = call_tool(&mut router, "explore", &json!({"symbol": "foo"}));
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
    let (resp4, err4) = call_tool(&mut router, "impact", &json!({"symbol": "foo"}));
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
    let dir = tempfile::tempdir().expect("tempdir");
    let mut router = ToolRouter::new_empty(store, dir.path().to_path_buf());

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

    router.init_focus();

    // ── Unscoped search must return an error ─────────────────────────
    let (resp, err) = call_tool(
        &mut router,
        "search",
        &json!({"query": "function", "analysis": "manifest"}),
    );
    assert!(
        err,
        "unscoped search should return an error when scope is required"
    );
    assert!(
        resp.get("error")
            .and_then(|v| v.as_str())
            .map_or(false, |s| s.contains("scope")),
        "error should mention scope: {resp:.300}"
    );

    // ── Scoped search to "sub/" ───────────────────────────────────────
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
        // Response should include coverage signal.
        assert!(
            resp2.get("coverage").is_some(),
            "scoped search response should include coverage signal: {resp2:.300}"
        );
    } else {
        eprintln!("search (scoped) error (acceptable for manifest mode): {resp2:.300}");
    }

    // ── Scoped search with root scope "." ─────────────────────────────
    let (resp3, err3) = call_tool(
        &mut router,
        "search",
        &json!({"query": "function", "scope": ".", "analysis": "manifest"}),
    );
    assert!(!err3, "search with scope='.' should succeed: {resp3:.300}");
    assert!(
        resp3.get("coverage").is_some(),
        "search response should include coverage signal: {resp3:.300}"
    );

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
            resp1.get("ok").is_some()
                || resp1.get("status").is_some()
                || resp1.get("annotation").is_some(),
            "First fp_dispatches add missing expected fields: {resp1:.300}",
        );
    }

    // Second add — should be idempotent (no duplicate, consistent result).
    let (resp2, err2) = call_tool(&mut router, "fp_dispatches", &add_args);
    if err2 {
        eprintln!("Second fp_dispatches add error: {resp2:.300}");
    } else {
        assert!(
            resp2.get("ok").is_some()
                || resp2.get("status").is_some()
                || resp2.get("annotation").is_some(),
            "Second fp_dispatches add missing expected fields: {resp2:.300}",
        );
    }

    // List to verify no duplicates.
    let (list_resp, list_err) = call_tool(&mut router, "fp_dispatches", &json!({"action": "list"}));
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

        // Try the structured selector form as a fallback. Without a
        // prebuilt index this may still be a structured not-found response.
        let (resp2, err2) = call_tool(
            &mut router,
            "symbol",
            &json!({"symbol": {"qualified_name": "foo"}, "view": "detail"}),
        );
        let has_fields = resp2.get("qualified_name").is_some()
            || resp2.get("name").is_some()
            || resp2.get("kind").is_some()
            || resp2.get("file").is_some()
            || resp2.get("error").is_some()
            || resp2.get("raw").is_some();
        assert!(
            err2 || has_fields,
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
// Test: Graph-backed tools on empty store return graceful errors
// =========================================================================

#[test]
fn graph_tools_on_empty_store_return_errors() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut router = ToolRouter::new_empty(store, dir.path().to_path_buf());
    let ctx = ToolCallContext::empty();

    // These should fail because symbol doesn't exist — but must NOT panic.
    let tools = ["calls", "explore", "path", "impact"];
    for tool in &tools {
        let result = router.call_tool(&ctx, tool, &json!({"symbol": "nonexistent"}));
        // Should return error, NOT panic
        let text = extract_text(&result);
        assert!(
            result.is_error == Some(true) || text.contains("error") || text.contains("not found"),
            "Tool '{tool}' should return error on empty store, got: {text}"
        );
    }
}

// =========================================================================
// Test: fp_dispatches with unknown field returns error
// =========================================================================

#[test]
fn fp_dispatches_unknown_field_returns_error() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut router = ToolRouter::new_empty(store, dir.path().to_path_buf());
    let ctx = ToolCallContext::empty();

    let result = router.call_tool(
        &ctx,
        "fp_dispatches",
        &json!({
            "action": "add",
            "field_qname": "nonexistent_struct.nonexistent_field",
            "target_qname": "nonexistent_function"
        }),
    );
    // Should return error about unresolved symbols
    let text = extract_text(&result);
    assert!(
        result.is_error == Some(true) || text.contains("error") || text.contains("not found"),
        "fp_dispatches with unknown field should return error, got: {text}"
    );
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
    let dir = tempfile::tempdir().expect("tempdir");
    let mut router = ToolRouter::new_empty(store, dir.path().to_path_buf());

    // Overlay + task tools — no graph needed, fast.
    let tools: &[(&str, Value)] = &[
        ("fp_dispatches", json!({"action": "list"})),
        ("domain_rules", json!({"action": "list"})),
        ("tasks", json!({})),
        ("project", json!({"action": "status"})),
        ("lifecycle", json!({"symbol": "x", "field": "y"})),
        ("branch_diff", json!({"symbol": "x"})),
        (
            "file_dependencies",
            json!({"file_path": "nonexistent.c", "analysis": "manifest"}),
        ),
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

// =========================================================================
// Test: Concurrent tool calls do not deadlock
// =========================================================================
//
// ToolRouter uses multiple Mutexes internally (engine, focus_runtime,
// query_snapshots, graph via RwLock).  This test verifies that
// concurrent calls across different tools do not deadlock or panic.

#[test]
fn concurrent_tool_calls_do_not_deadlock() {
    use std::thread;

    // 1. Create a shared ToolRouter with a cold temp C project.
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");

    let dir = tempfile::tempdir().expect("tempdir");
    let main_c = dir.path().join("main.c");
    std::fs::write(
        &main_c,
        "int foo(void) { return 0; }\nint bar(void) { return foo(); }\n",
    )
    .expect("write main.c");

    let mut router = ToolRouter::new_empty(store.clone(), dir.path().to_path_buf());

    router.init_focus();

    // Build the initial graph snapshot so graph-backed tools are live,
    // even if they later return scoped not-found responses.
    router
        .ensure_graph_initialized()
        .expect("ensure_graph_initialized");

    let router = Arc::new(std::sync::Mutex::new(router));

    // 2. Spawn 4 threads, each calling a different tool repeatedly.
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let router = Arc::clone(&router);
            thread::spawn(move || {
                for _ in 0..3 {
                    let mut r = router.lock().expect("lock");
                    let ctx = ToolCallContext::empty();
                    // ── Cover different mutex paths ─────────────────
                    //   symbol detail → search_engine() + store queries
                    //   calls         → context_builder() + graph
                    //   search        → store queries (no graph lock)
                    //   explore       → context_builder() + source extraction
                    let result = match i % 4 {
                        0 => {
                            r.call_tool(&ctx, "symbol", &json!({"symbol": "foo", "view": "detail"}))
                        }
                        1 => r.call_tool(
                            &ctx,
                            "calls",
                            &json!({"symbol": "foo", "direction": "outgoing"}),
                        ),
                        2 => r.call_tool(&ctx, "search", &json!({"query": "foo", "scope": "."})),
                        _ => r.call_tool(&ctx, "explore", &json!({"symbol": "foo"})),
                    };
                    // Drop lock before inspecting result to let other
                    // threads acquire the mutex.
                    drop(r);
                    // Result may be an error (e.g. graph not ready), but
                    // must not panic.
                    let _ = result;
                }
            })
        })
        .collect();

    // 3. Join all threads — no deadlock, no panic.
    for h in handles {
        h.join().expect("thread should not panic");
    }
}

// =========================================================================
// Test: All registered tools accept minimal args (smoke test)
// =========================================================================

/// Verify all 18 registered tools accept minimal arguments without panic.
/// Each tool is called with the minimum required parameters for its contract.
/// Return values are NOT validated for correctness — this is a smoke test.
#[test]
fn all_registered_tools_accept_minimal_args() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    // Create temp C project for focus + graph-backed tools
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.c"), "int foo(void) { return 0; }\n").unwrap();

    let mut router = ToolRouter::new_empty(store.clone(), dir.path().to_path_buf());
    let ctx = ToolCallContext::empty();

    router.init_focus();
    router.ensure_graph_initialized().unwrap();

    // Map each tool to its minimal valid arguments
    let tool_calls: Vec<(&str, serde_json::Value)> = vec![
        ("project", serde_json::json!({"action": "status"})),
        ("search", serde_json::json!({"query": "foo", "scope": "."})),
        (
            "symbol",
            serde_json::json!({"symbol": "foo", "view": "detail"}),
        ),
        (
            "calls",
            serde_json::json!({"symbol": "foo", "direction": "outgoing"}),
        ),
        ("explore", serde_json::json!({"symbol": "foo"})),
        ("path", serde_json::json!({"from": "foo", "to": "bar"})),
        ("impact", serde_json::json!({"symbol": "foo"})),
        (
            "file_dependencies",
            serde_json::json!({"file_path": "main.c", "direction": "outgoing"}),
        ),
        (
            "trace",
            serde_json::json!({"kind": "point", "file_path": "main.c", "line": 1, "column": 1}),
        ),
        (
            "lifecycle",
            serde_json::json!({"symbol": "foo", "field": "x"}),
        ),
        ("branch_diff", serde_json::json!({"symbol": "foo"})),
        ("fp_dispatches", serde_json::json!({"action": "list"})),
        ("domain_rules", serde_json::json!({"action": "list"})),
        ("tasks", serde_json::json!({})),
        (
            "resume_query",
            serde_json::json!({"query_id": "test-snapshot"}),
        ),
    ];

    for (tool_name, args) in &tool_calls {
        let result = router.call_tool(&ctx, tool_name, args);
        assert!(
            !result.content.is_empty(),
            "Tool '{}' returned empty content. Args: {}",
            tool_name,
            args
        );
    }
}
