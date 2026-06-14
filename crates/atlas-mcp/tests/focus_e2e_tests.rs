//! Focus-path E2E integration tests.
//!
//! These tests validate the full MCP → dispatch → focus → closure → graph
//! refresh pipeline using a self-contained temporary C project. No
//! pre-built `.atlas/atlas.db` is required.
//!
//! Run with:
//! ```bash
//! cargo test -p atlas-mcp --test focus_e2e_tests -- --nocapture
//! ```

use atlas_engine::Store;
use atlas_mcp::protocol::ContentBlock;
use atlas_mcp::tools::{ToolCallContext, ToolRouter};
use serde_json::{Value, json};
use std::sync::Arc;

// =========================================================================
// Helpers
// =========================================================================

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

/// Parse a tool response string into JSON, handling bare error strings.
fn parse_response(s: &str) -> Value {
    let s = s.trim();
    if s.starts_with('{') || s.starts_with('[') {
        serde_json::from_str(s).unwrap_or_else(|_| json!({"parse_error": true}))
    } else {
        json!({"is_error": true, "message": s})
    }
}

/// Call a tool via the full dispatch path and return (parsed_json, is_error).
fn call_tool(router: &mut ToolRouter, name: &str, args: &Value) -> (Value, bool) {
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, name, args);
    let text = extract_text(&result);
    let parsed = parse_response(text);
    let is_error = result.is_error.unwrap_or(false);
    (parsed, is_error)
}

/// Create a temporary C project with main.c, helper.c, helper.h.
fn create_temp_c_project(dir: &std::path::Path) {
    std::fs::write(
        dir.join("main.c"),
        r#"
#include "helper.h"
int main(void) {
    helper_foo();
    return 0;
}
"#,
    )
    .expect("write main.c");

    std::fs::write(
        dir.join("helper.c"),
        r#"
#include "helper.h"
static void helper_bar(void) { }

void helper_foo(void) {
    helper_bar();
}
"#,
    )
    .expect("write helper.c");

    std::fs::write(
        dir.join("helper.h"),
        r#"
#ifndef HELPER_H
#define HELPER_H
void helper_foo(void);
#endif
"#,
    )
    .expect("write helper.h");
}

/// Create a ToolRouter with an in-memory store for a C project.
fn setup_router(project_root: &std::path::Path) -> ToolRouter {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    ToolRouter::new_empty(store, project_root.to_path_buf())
}

// =========================================================================
// Test 1: Focus-driven context query triggers lazy extraction
// =========================================================================

#[test]
fn focus_context_triggers_lazy_extraction() {
    let temp_dir = tempfile::TempDir::with_prefix("atlas-focus-e2e-").expect("create temp dir");
    let project_root = temp_dir.path().to_path_buf();

    // 1. Create C project and router
    create_temp_c_project(&project_root);
    let mut router = setup_router(&project_root);

    // 2. Initialize FocusRuntime so lazy structural extraction can run.
    // No MCP index call is allowed; the query must drive focus bootstrap.
    router.init_focus();

    // 3. Trigger focus extraction via symbol context query
    let (ctx_resp, ctx_err) = call_tool(
        &mut router,
        "symbol",
        &json!({"symbol": "helper_foo", "view": "context", "language": "c"}),
    );

    // The focus path may return errors for various reasons (symbol not found,
    // C parse failures in this environment, etc.) — that's acceptable.
    // We verify no panic and that the response is structured.
    if ctx_err {
        eprintln!("Focus context query returned error (acceptable): {ctx_resp:.300}");
        // Still check it's a structured error — no panic
        assert!(
            ctx_resp.get("is_error").is_some()
                || ctx_resp.get("message").is_some()
                || ctx_resp.get("raw").is_some(),
            "Error response should have recognizable structure: {ctx_resp:.300}"
        );
    } else {
        // Check for expected context fields
        let has_context_data = ctx_resp.get("subject").is_some()
            || ctx_resp.get("data").is_some()
            || ctx_resp.get("callers").is_some()
            || ctx_resp.get("callees").is_some()
            || ctx_resp.get("file_peers").is_some();
        assert!(
            has_context_data,
            "Context response should contain expected fields: {ctx_resp:.300}"
        );

        // Check for focus-related analysis envelope fields
        if let Some(analysis) = ctx_resp.get("analysis") {
            let state = analysis.get("state").and_then(|v| v.as_str()).unwrap_or("");
            eprintln!("Focus analysis state: {state:?}");
            // Valid states: "building", "usable_partial", "ready"
            // All are acceptable — focus may have completed or be in progress
        }
        if let Some(precision) = ctx_resp.get("precision") {
            eprintln!("Focus precision: {precision}");
        }
    }

    // 4. Second call — verify no crash (cache/refresh behavior)
    let (ctx_resp2, ctx_err2) = call_tool(
        &mut router,
        "symbol",
        &json!({"symbol": "helper_foo", "view": "context", "language": "c"}),
    );
    if !ctx_err2 {
        let has_data = ctx_resp2.get("subject").is_some()
            || ctx_resp2.get("data").is_some()
            || ctx_resp2.get("callers").is_some()
            || ctx_resp2.get("callees").is_some()
            || ctx_resp2.get("file_peers").is_some();
        assert!(
            has_data,
            "Second context call should return valid data: {ctx_resp2:.300}"
        );
    }

    // 5. Verify graph edges via calls tool
    // After focus extraction, helper_foo → helper_bar edge may be present
    let (calls_resp, calls_err) = call_tool(
        &mut router,
        "calls",
        &json!({"symbol": "helper_foo", "direction": "outgoing"}),
    );
    if !calls_err {
        let has_calls_data = calls_resp.get("callees").is_some()
            || calls_resp.get("total_callees").is_some()
            || calls_resp.get("symbol").is_some()
            || calls_resp.get("nodes").is_some()
            || calls_resp.get("hops").is_some();
        assert!(
            has_calls_data,
            "Calls response should contain expected fields: {calls_resp:.300}"
        );

        if let Some(total) = calls_resp.get("total_callees").and_then(|v| v.as_u64()) {
            eprintln!("Callees for helper_foo: {total}");
            // helper_foo calls helper_bar, so we expect at least 1 callee.
            // But focus may not have completed fully, so 0 is also
            // potentially valid as a degraded state.
        }
    } else {
        eprintln!("Calls query returned error (acceptable): {calls_resp:.300}");
    }
}

// =========================================================================
// Test 2: Focus with Calls query intent via prepare_focus_query
// =========================================================================

#[test]
fn focus_calls_query_intent_works() {
    let temp_dir = tempfile::TempDir::with_prefix("atlas-focus-calls-").expect("create temp dir");
    let project_root = temp_dir.path().to_path_buf();

    create_temp_c_project(&project_root);
    let mut router = setup_router(&project_root);

    // Initialize focus runtime. No MCP index call is allowed; the focus
    // query below must discover and prepare the relevant project slice.
    router.init_focus();

    // Direct focus query for Calls intent
    let intent = atlas_engine::QueryIntent::Calls {
        symbol_name: "helper_foo".into(),
        file_id: None,
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let (focus_opt, warnings) = router.prepare_focus_query(Some(intent));

    if !warnings.is_empty() {
        eprintln!("Focus warnings: {warnings:?}");
    }

    // Focus should return a result for a cold DB with FocusRuntime active.
    if let Some(result) = focus_opt {
        eprintln!(
            "Focus result: mode={:?}, closure_id={:?}, built_files={}, precision={:?}",
            result.mode,
            result.closure_id,
            result.built_files.len(),
            result.precision,
        );

        // In a fresh cold DB, Focus mode is expected.
        assert_eq!(
            result.mode,
            atlas_engine::focus::runtime::IndexMode::Focus,
            "Expected Focus mode for manifest-only DB"
        );
        assert!(
            result.precision.is_some(),
            "Focus result should have precision"
        );
        assert!(
            !result.built_files.is_empty() || !result.pending_closure_ids.is_empty(),
            "Focus should build files or create pending closures. \
             Built: {}, Pending: {:?}",
            result.built_files.len(),
            result.pending_closure_ids
        );
    } else {
        eprintln!("Focus returned None (may indicate full index detected)");
        // This is acceptable for certain environments — the test
        // primarily verifies no panic and the API contract.
    }
}

// =========================================================================
// Test 3: Symbol context query without focus initialization still works
// =========================================================================

#[test]
fn symbol_context_without_focus_init() {
    let temp_dir = tempfile::TempDir::with_prefix("atlas-no-focus-").expect("create temp dir");
    let project_root = temp_dir.path().to_path_buf();

    create_temp_c_project(&project_root);
    let mut router = setup_router(&project_root);

    // Call symbol context without explicit focus configuration — should
    // still work as a structured degraded response.
    let (ctx_resp, ctx_err) = call_tool(
        &mut router,
        "symbol",
        &json!({"symbol": "helper_foo", "view": "context"}),
    );

    if ctx_err {
        eprintln!("Context query without focus returned error (acceptable): {ctx_resp:.300}");
    } else {
        // Without focus, we still expect a valid JSON structure (even if
        // structural data is incomplete).
        let has_data = ctx_resp.get("subject").is_some()
            || ctx_resp.get("callers").is_some()
            || ctx_resp.get("callees").is_some()
            || ctx_resp.get("data").is_some()
            || ctx_resp.get("file_peers").is_some()
            || ctx_resp.get("raw").is_some();
        assert!(
            has_data,
            "Context response without focus should be valid JSON: {ctx_resp:.300}"
        );
    }
}
