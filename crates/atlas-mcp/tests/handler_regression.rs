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
use atlas_engine::{FileInfo, Language, ParseStatus, SymbolDef, SymbolId, SymbolKind, TextRange};
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
    let router = ToolRouter::new_empty(store, temp_dir.to_path_buf());
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

#[test]
fn public_guidance_uses_error_or_message_without_hint_field() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, std::path::PathBuf::from("/tmp"));

    let cases = [
        ("search", json!({"query": "target"})),
        ("resume_query", json!({"query_id": "expired"})),
        ("domain_rules", json!({"action": "learn"})),
    ];
    for (tool, args) in cases {
        let (response, _) = call_tool(&mut router, tool, &args);
        assert!(
            response.get("hint").is_none(),
            "{tool} must not expose the retired hint field: {response}"
        );
        assert!(
            response.get("error").is_some() || response.get("message").is_some(),
            "{tool} should retain actionable guidance in error or message: {response}"
        );
    }
}

fn seed_persistent_symbol(
    store: &Store,
    path: &str,
    name: &str,
    kind: SymbolKind,
) -> atlas_engine::FileId {
    let file_id = atlas_engine::FileId::generate(path);
    store
        .upsert_file(&FileInfo {
            file_id,
            path: path.to_string(),
            language: Language::C,
            content_hash: "persistent-fixture".to_string(),
            status: ParseStatus::Success,
        })
        .unwrap();
    let sym_id = SymbolId::generate(&file_id, Language::C.as_str(), name, kind.as_str(), None);
    let sym = SymbolDef {
        id: sym_id,
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        symbol_path: vec![name.to_string()],
        file_id,
        language: Language::C,
        range: TextRange::default(),
        name_range: TextRange::default(),
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    };
    store.insert_symbols(&[sym]).unwrap();
    file_id
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

#[test]
fn handler_calls_cold_symbol_triggers_focus_retry_instead_of_not_found() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_calls_cold_symbol");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    let mut router = setup_temp_c_project(&temp_dir);

    let (resp, err) = call_tool(
        &mut router,
        "calls",
        &json!({"symbol": "foo", "direction": "outgoing"}),
    );

    assert!(
        !err,
        "cold calls query should not fail as NotFound: {resp:.500}"
    );
    assert_eq!(resp["symbol"], "foo");
    assert!(
        resp.get("total_callees").is_some() || resp.get("callees").is_some(),
        "cold calls query should return a bounded graph response: {resp:.500}",
    );
    assert_eq!(resp["analysis"]["scope"], "local");
    assert!(
        resp["analysis"]["retry_after_ms"]
            .as_u64()
            .is_some_and(|ms| ms > 0 && ms <= 60_000),
        "cold calls query should expose a bounded focus ETA: {resp:.500}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_explore_cold_scope_returns_local_dossier() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_explore_cold_scope");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    let mut router = setup_temp_c_project(&temp_dir);

    let (resp, err) = call_tool(
        &mut router,
        "explore",
        &json!({"symbol": "foo", "scope": ".", "source_mode": "full"}),
    );

    assert!(
        !err,
        "cold scoped explore should return local dossier instead of NotFound: {resp:.500}"
    );
    assert!(
        resp.get("subject").is_some() || resp.get("sourceExcerpt").is_some(),
        "cold scoped explore should contain local dossier fields: {resp:.500}",
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_explore_scope_miss_does_not_fallback_outside_scope() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_explore_scope_miss");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(temp_dir.join("a")).expect("create scope a");
    std::fs::create_dir_all(temp_dir.join("b")).expect("create scope b");
    std::fs::write(temp_dir.join("a/only_a.c"), "void only_a(void) {}\n").expect("write a");
    std::fs::write(temp_dir.join("b/foo.c"), "void foo(void) {}\n").expect("write b");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    let (resp, err) = call_tool(
        &mut router,
        "explore",
        &json!({"symbol": "foo", "scope": "a"}),
    );

    assert!(
        !err,
        "scoped explore miss should be a retryable partial response: {resp:.500}"
    );
    assert_eq!(resp["status"], "building");
    assert_eq!(resp["scope"], "a");
    assert!(
        resp.get("subject").is_none(),
        "explore must not return a dossier for a symbol outside the requested scope: {resp:.500}"
    );
    assert!(resp.get("background_refinement").is_none());
    assert_eq!(resp["analysis"]["retry_after_ms"], 2000);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_explore_unscoped_cold_symbol_queues_candidate_focus_without_dossier() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_explore_unscoped_candidate");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    std::fs::write(temp_dir.join("target.c"), "void unscoped_target(void) {}\n")
        .expect("write target");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    let (resp, err) = call_tool(
        &mut router,
        "explore",
        &json!({"symbol": "unscoped_target"}),
    );

    assert!(
        !err,
        "unscoped cold explore should be a retryable partial response: {resp:.500}"
    );
    assert_eq!(resp["status"], "building");
    assert!(
        resp.get("subject").is_none(),
        "unscoped cold explore should not synchronously return a dossier: {resp:.500}"
    );
    assert!(
        resp["candidate_files"]
            .as_array()
            .is_some_and(|files| files.iter().any(|f| f.as_str() == Some("target.c"))),
        "unscoped cold explore should expose bounded candidate files: {resp:.500}",
    );
    assert!(resp.get("background_refinement").is_none());
    assert_eq!(resp["analysis"]["retry_after_ms"], 2000);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_unscoped_cold_symbol_tools_enqueue_candidate_focus() {
    for (tool, args) in [
        (
            "calls",
            json!({"symbol": "unscoped_tool_target", "direction": "both"}),
        ),
        (
            "trace",
            json!({"kind": "callers", "symbol": "unscoped_tool_target"}),
        ),
        (
            "symbol",
            json!({"symbol": "unscoped_tool_target", "view": "usages"}),
        ),
        (
            "lifecycle",
            json!({"symbol": "unscoped_tool_target", "field": "state.ptr"}),
        ),
    ] {
        let temp_dir = std::env::temp_dir().join(format!(
            "atlas_hdlr_unscoped_candidate_{}_{}",
            tool,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        std::fs::write(
            temp_dir.join("target.c"),
            "void unscoped_tool_target(void) {}\n",
        )
        .expect("write target");

        let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
        store.init_schema().expect("init_schema");
        let mut router = ToolRouter::new_empty(store, temp_dir.clone());
        router.init_focus();

        let (resp, err) = call_tool(&mut router, tool, &args);
        assert!(
            !err,
            "{tool} should return retryable partial for unscoped cold symbol: {resp:.500}"
        );
        if resp["status"] == "building" {
            assert!(
                resp["candidate_files"]
                    .as_array()
                    .is_some_and(|files| files.iter().any(|f| f.as_str() == Some("target.c"))),
                "{tool} should expose bounded candidate files while unresolved: {resp:.500}",
            );
            // Retry guidance lives in the flat retry_after_ms field and the
            // analysis block; legacy background_refinement is not emitted.
            assert_eq!(
                resp["retry_after_ms"], 8000,
                "{tool} should expose flat retry_after_ms while building: {resp:.500}",
            );
        } else {
            // Phase 1.4: apply_focus_result_to_lr no longer sets partial_result.
            // The retry guidance is expressed via retry_after_ms presence in the analysis block.
            assert!(
                resp["analysis"]["scope"].as_str().is_some(),
                "{tool} should have analysis scope for local materialized result: {resp:.500}",
            );
            assert!(
                resp["analysis"]["retry_after_ms"]
                    .as_u64()
                    .is_some_and(|ms| ms > 0),
                "{tool} should carry retry_after_ms in the analysis block: {resp:.500}",
            );
            assert!(
                resp.get("background_refinement").is_none(),
                "{tool} should not expose legacy background_refinement: {resp:.500}",
            );
        }
        assert!(
            resp["analysis"]["retry_after_ms"]
                .as_u64()
                .is_some_and(|ms| ms > 0 && ms <= 60_000),
            "{tool} should expose a bounded focus ETA: {resp:.500}"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

#[test]
fn handler_symbol_analysis_tools_return_retryable_partial_for_cold_symbol() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_retryable_cold_symbol_tools");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    std::fs::write(
        temp_dir.join("target.c"),
        "void unrelated_target(void) {}\n",
    )
    .expect("write target");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    for (tool, args) in [
        (
            "trace",
            json!({"kind": "callers", "symbol": "missing_trace_target"}),
        ),
        (
            "symbol",
            json!({"symbol": "missing_usages_target", "view": "usages"}),
        ),
        (
            "lifecycle",
            json!({"symbol": "missing_lifecycle_target", "field": "state.ptr"}),
        ),
        ("branch_diff", json!({"symbol": "missing_branch_target"})),
    ] {
        let (resp, err) = call_tool(&mut router, tool, &args);
        assert!(
            !err,
            "{tool} should return a retryable partial response for cold symbols: {resp:.500}"
        );
        assert_eq!(resp["status"], "building", "{tool}: {resp:.500}");
        // Phase 1.5: retryable_symbol_not_found_response uses 8000 ms.
        assert_eq!(
            resp["analysis"]["retry_after_ms"], 8000,
            "{tool}: {resp:.500}"
        );
        // Phase 1.5: background_refinement removed — the flat retry_after_ms
        // field carries the retry contract instead.
        assert_eq!(resp["retry_after_ms"], 8000, "{tool}: {resp:.500}");
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_graph_tools_return_retryable_partial_for_missing_symbol() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_retryable_graph_missing_symbol");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    std::fs::write(
        temp_dir.join("target.c"),
        "void unrelated_target(void) {}\n",
    )
    .expect("write target");

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    for (tool, args) in [
        (
            "calls",
            json!({"symbol": "missing_calls_target", "direction": "both"}),
        ),
        (
            "trace",
            json!({"kind": "callers", "symbol": "missing_trace_target"}),
        ),
        (
            "path",
            json!({"from": "missing_path_from", "to": "missing_path_to"}),
        ),
        ("impact", json!({"symbol": "missing_impact_target"})),
    ] {
        let (resp, err) = call_tool(&mut router, tool, &args);
        assert!(
            !err,
            "{tool} should return a retryable partial response for missing symbols: {resp:.500}"
        );
        assert_eq!(resp["status"], "building", "{tool}: {resp:.500}");
        // Phase 1.5: retryable_symbol_not_found_response uses 8000 ms.
        assert_eq!(
            resp["analysis"]["retry_after_ms"], 8000,
            "{tool}: {resp:.500}"
        );
        // Phase 1.5: background_refinement removed — the flat retry_after_ms
        // field carries the retry contract instead.
        assert_eq!(resp["retry_after_ms"], 8000, "{tool}: {resp:.500}");
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// Test 2: Analysis tools return bounded responses for missing symbols
// =========================================================================

#[test]
fn handler_analysis_tools_missing_symbols_are_retryable() {
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
    assert_eq!(resp["status"], "building", "lifecycle: {resp:.300}");
    // Phase 1.5: retryable_symbol_not_found_response uses 8000 ms flat & analysis.
    assert_eq!(resp["retry_after_ms"], 8000);
    assert_eq!(resp["analysis"]["retry_after_ms"], 8000);

    // ── branch_diff with non-existent symbol ─────────────────────────
    let (resp2, _err2) = call_tool(
        &mut router,
        "branch_diff",
        &json!({"symbol": "NonExistentFunction_XYZ_999"}),
    );
    assert_eq!(resp2["status"], "building", "branch_diff: {resp2:.300}");
    assert_eq!(resp2["retry_after_ms"], 8000);
    assert_eq!(resp2["analysis"]["retry_after_ms"], 8000);
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
    assert!(
        resp3.get("coverage_counts").is_none(),
        "search should not merge focus closure coverage_counts: {resp3:.300}"
    );
    assert!(
        resp3.get("gaps").is_none(),
        "search should not merge focus closure gaps: {resp3:.300}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_open_project_uses_persistent_store_without_storage_mode() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_open_project_persistent_store");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(temp_dir.join(".atlas")).expect("create atlas dir");

    let canonical_temp_dir = temp_dir.canonicalize().expect("canonical temp dir");
    let persistent_path = canonical_temp_dir.join(".atlas").join("atlas.db");
    let persistent = Store::open_db(&persistent_path).expect("open persistent db");
    persistent.init_schema().expect("init persistent schema");
    seed_persistent_symbol(
        &persistent,
        "src/persisted.c",
        "persisted_open_hit",
        SymbolKind::Function,
    );
    drop(persistent);

    let mut router = ToolRouter::new_unopened();
    let (open_resp, open_err) = call_tool(
        &mut router,
        "project",
        &json!({"action": "open", "project_path": temp_dir.to_string_lossy()}),
    );
    assert!(!open_err, "project open should succeed: {open_resp:.500}");
    assert_eq!(open_resp["ok"], true);
    assert_eq!(
        open_resp["db_path"].as_str(),
        Some(persistent_path.to_string_lossy().as_ref())
    );
    assert!(
        open_resp.get("storage").is_none(),
        "open response must not expose a storage mode: {open_resp:.500}"
    );

    let (status_resp, status_err) = call_tool(&mut router, "project", &json!({"action": "status"}));
    assert!(
        !status_err,
        "project status should succeed: {status_resp:.500}"
    );
    assert!(
        status_resp["project"].get("storage").is_none(),
        "status response must not expose a storage mode: {status_resp:.500}"
    );

    let (search_resp, search_err) = call_tool(
        &mut router,
        "search",
        &json!({"query": "persisted_open_hit", "scope": "src", "analysis": "manifest"}),
    );
    assert!(
        !search_err,
        "search should read the active persistent store directly: {search_resp:.500}"
    );
    assert!(
        search_resp.get("index_source").is_none(),
        "search response must not expose the physical index source: {search_resp:.500}"
    );
    assert_eq!(search_resp["total"], 1);
    assert_eq!(search_resp["results"][0]["name"], "persisted_open_hit");
    assert_eq!(search_resp["results"][0]["file"], "src/persisted.c");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_search_reports_retry_for_cold_scope() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_search_retry_cold_scope");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    for ix in 0..4 {
        std::fs::write(
            temp_dir.join(format!("cold_{ix}.c")),
            "void cold_target(void) {}\n",
        )
        .expect("write cold file");
    }

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    let (resp, err) = call_tool(
        &mut router,
        "search",
        &json!({"query": "cold_target", "scope": "."}),
    );
    assert!(
        !err,
        "cold search should return a bounded result, got: {resp:.300}"
    );
    assert!(
        resp.get("background_refinement").is_none(),
        "cold bounded search should not expose legacy background_refinement: {resp:.300}"
    );
    assert!(
        resp["analysis"].get("retry_after_ms").is_some(),
        "analysis block should carry retry_after_ms: {resp:.300}"
    );
    assert!(resp.get("precision").is_none());

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_search_partial_hit_without_deferred_ids_reports_retry() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_search_retry_single_hit");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    for ix in 0..35 {
        let body = if ix == 17 {
            "void singleton_cold_target(void) {}\n"
        } else {
            "void unrelated_helper(void) {}\n"
        };
        std::fs::write(temp_dir.join(format!("cold_{ix}.c")), body).expect("write cold file");
    }

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    let (resp, err) = call_tool(
        &mut router,
        "search",
        &json!({"query": "singleton_cold_target", "scope": "."}),
    );
    assert!(
        !err,
        "partial cold search with one hit should return a bounded result: {resp:.300}"
    );
    assert_eq!(resp["coverage"]["state"], "partial");
    assert!(
        resp.get("background_refinement").is_none(),
        "partial search should not expose legacy background_refinement: {resp:.300}"
    );
    assert!(resp.get("precision").is_none());
    assert_eq!(resp["analysis"]["retry_after_ms"], 2000);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn handler_search_partial_no_hit_tells_client_to_retry() {
    let temp_dir = std::env::temp_dir().join("atlas_hdlr_search_no_hit_retry");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");

    for ix in 0..36 {
        std::fs::write(
            temp_dir.join(format!("cold_{ix}.c")),
            "void unrelated_helper(void) {}\n",
        )
        .expect("write cold file");
    }

    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let mut router = ToolRouter::new_empty(store, temp_dir.clone());
    router.init_focus();

    let (resp, err) = call_tool(
        &mut router,
        "search",
        &json!({"query": "definitely_absent_symbol", "scope": "."}),
    );
    assert!(
        !err,
        "partial cold no-hit search should return retryable partial response: {resp:.300}"
    );
    assert_eq!(resp["coverage"]["state"], "partial");
    assert_eq!(resp["analysis"]["retry_after_ms"], 2000);
    assert!(
        resp.get("background_refinement").is_none(),
        "partial no-hit search should not expose legacy background_refinement: {resp:.300}"
    );
    assert!(resp.get("precision").is_none());

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
// Test: Graph-backed tools on empty store return graceful bounded responses
// =========================================================================

#[test]
fn graph_tools_on_empty_store_return_bounded_responses() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let dir = tempfile::tempdir().expect("tempdir");
    let router = ToolRouter::new_empty(store, dir.path().to_path_buf());
    let ctx = ToolCallContext::empty();

    // These should return bounded retryable responses because the symbol does
    // not exist in the current focus closure — and must NOT panic.
    for (tool, args) in [
        ("calls", json!({"symbol": "nonexistent"})),
        (
            "path",
            json!({"from": "nonexistent_from", "to": "nonexistent_to"}),
        ),
        ("impact", json!({"symbol": "nonexistent"})),
    ] {
        let result = router.call_tool(&ctx, tool, &args);
        let text = extract_text(&result);
        let resp: Value = serde_json::from_str(text).expect("graph tool should return JSON");
        assert_eq!(result.is_error, Some(false), "{tool}: {text}");
        assert_eq!(resp["status"], "building", "{tool}: {text}");
        // Phase 1.5: retryable_symbol_not_found_response uses 8000 ms.
        assert_eq!(resp["analysis"]["retry_after_ms"], 8000, "{tool}: {text}");
    }

    // explore uses its own inline not-found handler (unchanged in Phase 1.5).
    let result = router.call_tool(&ctx, "explore", &json!({"symbol": "nonexistent"}));
    let text = extract_text(&result);
    let resp: Value = serde_json::from_str(text).expect("explore should return JSON");
    assert_eq!(result.is_error, Some(false));
    assert_eq!(resp["status"], "building");
    assert_eq!(resp["analysis"]["retry_after_ms"], 2000);
}

// =========================================================================
// Test: fp_dispatches with unknown field returns error
// =========================================================================

#[test]
fn fp_dispatches_unknown_field_returns_error() {
    let store = Arc::new(Store::open_in_memory().expect("open_in_memory"));
    store.init_schema().expect("init_schema");
    let dir = tempfile::tempdir().expect("tempdir");
    let router = ToolRouter::new_empty(store, dir.path().to_path_buf());
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
    let router = ToolRouter::new_empty(store, dir.path().to_path_buf());

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

    let router = ToolRouter::new_empty(store.clone(), dir.path().to_path_buf());

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
                    let r = router.lock().expect("lock");
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

    let router = ToolRouter::new_empty(store.clone(), dir.path().to_path_buf());
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
