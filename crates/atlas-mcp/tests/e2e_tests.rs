//! End-to-end tests against example projects.
//!
//! These tests iterate over `examples/` directories that have `.atlas/` DBs
//! and run a generic tool battery. No hardcoded symbol names or search
//! queries — the tests adapt to whatever content each example repo has.
//!
//! Run with:
//! ```bash
//! cargo test -p atlas-mcp --test e2e_tests -- --test-threads=1
//! ```

use atlas_engine::Store;
use atlas_mcp::tools::{ToolCallContext, ToolRouter};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Discover example directories that have `.atlas/atlas.db`.
fn discover_example_projects() -> Vec<PathBuf> {
    let examples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut projects = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&examples_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && path.join(".atlas/atlas.db").exists()
                && let Ok(canonical) = path.canonicalize()
            {
                projects.push(canonical);
            }
        }
    }
    projects
}

/// Create a ToolRouter with graph initialized for the given project.
fn open_router(project_root: &std::path::Path) -> ToolRouter {
    let db_path = project_root.join(".atlas/atlas.db");
    let store = Store::open_db(&db_path)
        .unwrap_or_else(|e| panic!("Failed to open DB at {db_path:?}: {e}"));
    let store = std::sync::Arc::new(store);
    let router = ToolRouter::new_empty(store, project_root.to_path_buf());
    router
        .ensure_graph_initialized()
        .unwrap_or_else(|e| panic!("Failed to init graph for {project_root:?}: {e}"));
    router
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

/// Call a tool and return the parsed JSON response + is_error flag.
fn call_tool(router: &mut ToolRouter, name: &str, args: &Value) -> (Value, bool) {
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, name, args);
    let text = result
        .content
        .first()
        .map(|cb| match cb {
            atlas_mcp::protocol::ContentBlock::Text { text } => text.as_str(),
        })
        .unwrap_or("");
    let parsed = parse_response(text);
    let is_error = result.is_error.unwrap_or(false);
    (parsed, is_error)
}

/// Get the first search result's qualified_name and file, or None.
fn pick_first_result(search_response: &Value) -> Option<(&str, &str)> {
    search_response
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|hit| {
            let qname = hit.get("qualified_name").and_then(|v| v.as_str())?;
            let file = hit.get("file").and_then(|v| v.as_str()).unwrap_or("");
            Some((qname, file))
        })
}

// =========================================================================
// Main e2e battery — iterates over ALL example projects with .atlas/ DBs
// =========================================================================

#[test]
fn e2e_all_example_projects() {
    let projects = discover_example_projects();
    assert!(
        !projects.is_empty(),
        "No example projects with .atlas/atlas.db found. \
         Run 'atlas index' in at least one example project."
    );

    for project_root in &projects {
        let project_name = project_root.file_name().unwrap().to_string_lossy();
        eprintln!("\n=== Testing project: {project_name} ===");
        let mut router = open_router(project_root);

        // ── 1. status ─────────────────────────────────────────────────
        let (status, is_err) = call_tool(&mut router, "project", &json!({"action": "status"}));
        assert!(!is_err, "[{project_name}] status failed: {status:.300}");
        assert!(
            status.get("project").is_some() || status.get("summary").is_some(),
            "[{project_name}] status missing project/summary: {status:.300}"
        );

        // ── 2. search with broad queries ──────────────────────────────
        let mut found_qname: Option<String> = None;
        let mut found_file: Option<String> = None;

        for broad_query in &[
            "a", "the", "fn", "class", "def", "int", "struct", "func", "void", "let",
        ] {
            let (resp, is_err) = call_tool(
                &mut router,
                "search",
                &json!({"query": broad_query, "analysis": "manifest"}),
            );
            if is_err {
                continue;
            }
            if let Some((qname, file)) = pick_first_result(&resp) {
                found_qname = Some(qname.to_string());
                if !file.is_empty() {
                    found_file = Some(file.to_string());
                }
                break;
            }
        }

        // Accept projects where broad search finds nothing (e.g. very small)
        if found_qname.is_none() {
            eprintln!(
                "[{project_name}] No search results for any broad query — skipping symbol-dependent tests"
            );
            // Still run infrastructure tests
        }

        // ── 3. symbol (detail + context) on found result ──────────────
        if let Some(ref qname) = found_qname {
            // detail
            let (sym_detail, is_err) = call_tool(
                &mut router,
                "symbol",
                &json!({"symbol": qname, "view": "detail"}),
            );
            if !is_err {
                assert!(
                    sym_detail.get("qualified_name").is_some(),
                    "[{project_name}] symbol detail missing qualified_name: {sym_detail:.300}"
                );
            }
            // context
            let (sym_ctx, is_err) = call_tool(
                &mut router,
                "symbol",
                &json!({"symbol": qname, "view": "context"}),
            );
            if !is_err {
                // Context response may have data at top level or in "data" field
                let has_data = sym_ctx.get("data").is_some()
                    || sym_ctx.get("file_peers").is_some()
                    || sym_ctx.get("callers").is_some()
                    || sym_ctx.get("callees").is_some()
                    || sym_ctx.get("resolved").is_some()
                    || sym_ctx.get("subject").is_some();
                assert!(
                    has_data,
                    "[{project_name}] symbol context empty: {sym_ctx:.300}"
                );
            }
        }

        // ── 4. impact on found symbol ─────────────────────────────────
        if let Some(ref qname) = found_qname {
            let (impact, is_err) = call_tool(&mut router, "impact", &json!({"symbol": qname}));
            if !is_err {
                let has_impact = impact.get("file_groups").is_some()
                    || impact.get("impacted").is_some()
                    || impact.get("symbol").is_some();
                assert!(
                    has_impact,
                    "[{project_name}] impact missing expected fields: {impact:.300}"
                );
            }
        }

        // ── 5. explore on found symbol ────────────────────────────────
        if let Some(ref qname) = found_qname {
            let (explore, is_err) = call_tool(&mut router, "explore", &json!({"symbol": qname}));
            if !is_err {
                let has_data =
                    explore.get("callEvidence").is_some() || explore.get("fileContext").is_some();
                assert!(
                    has_data,
                    "[{project_name}] explore missing callEvidence/fileContext: {explore:.300}"
                );
            }
        }

        // ── 6. calls (callers + callees) on found symbol ──────────────
        if let Some(ref qname) = found_qname {
            for direction in &["incoming", "outgoing"] {
                let (resp, is_err) = call_tool(
                    &mut router,
                    "calls",
                    &json!({"symbol": qname, "direction": direction}),
                );
                if !is_err {
                    let has_data = resp.get("callers").is_some()
                        || resp.get("callees").is_some()
                        || resp.get("nodes").is_some()
                        || resp.get("hops").is_some();
                    assert!(
                        has_data,
                        "[{project_name}] calls({direction}) missing expected fields: {resp:.300}"
                    );
                }
            }
        }

        // ── 7. file_dependencies with found file ──────────────────────
        if let Some(ref file) = found_file {
            for direction in &["outgoing", "incoming", "both"] {
                let (resp, is_err) = call_tool(
                    &mut router,
                    "file_dependencies",
                    &json!({"file_path": file, "direction": direction, "analysis": "manifest"}),
                );
                if !is_err {
                    let has_data = resp.get("dependencies").is_some()
                        || resp.get("dependents").is_some()
                        || resp.get("outgoing").is_some()
                        || resp.get("incoming").is_some();
                    assert!(
                        has_data,
                        "[{project_name}] file_deps({direction}) missing deps: {resp:.300}"
                    );
                }
            }
        }

        // ── 8. infrastructure tools ───────────────────────────────────
        let infra_tools: &[(&str, Value)] = &[
            ("domain_rules", json!({"action": "list"})),
            ("tasks", json!({})),
            ("fp_dispatches", json!({"action": "list"})),
        ];

        for (name, args) in infra_tools {
            let (resp, _is_err) = call_tool(&mut router, name, args);
            // Just verify no panic — response structure varies
            let _ = resp;
        }

        // ── 9. trace — verify no panic (accept errors) ────────────────
        if let Some(ref qname) = found_qname {
            for kind in &["callers", "forward"] {
                let args = match *kind {
                    "callers" => json!({"kind": "callers", "symbol": qname}),
                    "forward" => json!({"kind": "forward", "from": qname, "to": qname}),
                    _ => continue,
                };
                let (_resp, _is_err) = call_tool(&mut router, "trace", &args);
                // Accept any outcome — just verify no panic
            }
        }

        // ── 10. all tools with minimal args — no panic ────────────────
        let all_tools: &[(&str, Value)] = &[
            ("project", json!({"action": "status"})),
            ("fp_dispatches", json!({"action": "list"})),
            ("domain_rules", json!({"action": "list"})),
            ("tasks", json!({})),
            ("resume_query", json!({"query_id": "nonexistent"})),
        ];

        for (name, args) in all_tools {
            let (_resp, _is_err) = call_tool(&mut router, name, args);
            // Just verify no panic
        }

        // ── 11. lifecycle / branch_diff with gibberish — no panic ─────
        for tool_name in &["lifecycle", "branch_diff"] {
            let (_resp, _is_err) = call_tool(
                &mut router,
                tool_name,
                &json!({"symbol": "NonExistent_XYZ123"}),
            );
        }

        eprintln!("[{project_name}] All tests passed\n");
    }
}

// =========================================================================
// FocusRuntime e2e test — python_example specific
// =========================================================================

#[test]
fn e2e_focus_runtime_python_example() {
    let examples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let python_root = examples_dir
        .join("python_example")
        .canonicalize()
        .expect("python_example directory not found");
    let db_path = python_root.join(".atlas/atlas.db");
    assert!(db_path.exists(), "python_example .atlas/atlas.db not found");

    let store =
        std::sync::Arc::new(Store::open_db(&db_path).expect("Failed to open python_example DB"));
    let router = ToolRouter::new_empty(store, python_root.clone());
    router
        .ensure_graph_initialized()
        .expect("Failed to init graph");

    // 2. prepare_focus_query with Calls intent
    let intent = atlas_engine::QueryIntent::Calls {
        symbol_name: "WikipediaSpider".into(),
        file_id: None,
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let (focus_opt, _warnings) = router.prepare_focus_query(Some(intent));

    // 3. Verify focus response has mode and precision. closure_id is an
    // internal scheduler detail and must not be part of the public MCP
    // contract.
    if let Some(result) = focus_opt {
        // Focus mode is expected for manifest-only DB
        assert!(
            result.quality.is_some()
                || result.access == atlas_engine::focus::runtime::AccessStrategy::FullCache,
            "Focus result should have precision or be FullIndex. precision={:?}",
            result.quality
        );
    }
    // focus_opt==None is acceptable: full index detected
}
