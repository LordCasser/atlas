use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;

// ── Helper: create an in-memory store with schema ────────────────────
fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

// ── Helper: register a minimal TypeScript file ──────────────────────
fn register_test_file(store: &Store, path: &str) -> FileId {
    let file_id = FileId::generate(path);
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id,
            path: path.into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    file_id
}

// ── Helper: insert a minimal function symbol ────────────────────────
fn insert_test_symbol(store: &Store, file_id: FileId, name: &str) {
    insert_test_symbol_with_signature(store, file_id, name, None, false, false);
}

fn insert_test_symbol_with_signature(
    store: &Store,
    file_id: FileId,
    name: &str,
    signature: Option<&str>,
    exported: bool,
    async_: bool,
) {
    let range = atlas_engine::TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 11,
    };
    let sym = atlas_engine::SymbolDef {
        id: SymbolId::generate(&file_id, "typescript", name, "function", None),
        kind: atlas_engine::SymbolKind::Function,
        name: name.into(),
        qualified_name: format!("{name}.{name}"),
        symbol_path: vec![name.into()],
        file_id,
        language: atlas_engine::Language::TypeScript,
        range,
        name_range: range,
        signature: signature.map(str::to_string),
        visibility: None,
        exported,
        static_: false,
        async_,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".into(),
    };
    store.insert_symbols(&[sym]).unwrap();
}

// ── ensure_graph_initialized mode detection ───────────────────────

#[test]
fn ensure_graph_initialized_detects_focus_partial_mode() {
    let store = test_store();
    // In-memory store with no index → read_catalog_tier returns empty/default,
    // which is not a rich index mode → FocusPartial.
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    assert!(
        !router
            .project()
            .graph_runtime
            .state
            .graph_initialized
            .load(Ordering::Acquire)
    );

    assert_eq!(
        *router.project().graph_runtime.provenance.lock().unwrap(),
        EdgeProvenance::FocusScoped,
        "fresh in-memory store should produce FocusPartial mode"
    );
}

#[test]
fn ensure_graph_initialized_detects_full_canonical_mode() {
    let store = test_store();
    // Register a file with a "structural" extraction state so
    // read_catalog_tier() returns a rich index mode.
    let file_id = register_test_file(&store, "test.ts");
    store
        .upsert_file_extraction_state(
            &file_id,
            "structural",
            "hash1",
            "complete",
            atlas_engine::structs::FactCoverage::default(),
        )
        .unwrap();

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    assert_eq!(
        *router.project().graph_runtime.provenance.lock().unwrap(),
        EdgeProvenance::RepoCanonical,
        "store with structural extraction should produce FullCanonical mode"
    );
}

// ── Phase 7a: resource preparation inside call_tool ────────────────

/// A graph tool call on a store without schema should fail inside
/// call_tool() with is_error=true and a descriptive message.
#[test]
fn graph_init_error_propagates_in_call_tool() {
    // Store without schema → GraphEngine::from_store will fail
    let store = Store::open_in_memory().unwrap();
    let router = ToolRouter::new_empty(Arc::new(store), PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({"symbol": "foo.bar"});
    let result = router.call_tool(&ctx, "calls", &args);

    assert_eq!(result.is_error, Some(true), "should be an error");
    let body = &result.content[0];
    let text = match body {
        ContentBlock::Text { text } => text,
    };
    assert!(
        text.contains("Failed to initialize graph snapshot"),
        "error text should mention graph init failure, got: {text}",
    );
}

/// A non-graph tool call should NOT trigger graph initialization,
/// even if the store has no schema (which would cause graph init to
/// fail were it attempted).
#[test]
fn call_tool_without_graph_init_for_non_graph_tool() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    assert!(
        !router
            .project()
            .graph_runtime
            .state
            .graph_initialized
            .load(std::sync::atomic::Ordering::Acquire),
        "graph should not be initialized yet",
    );

    let ctx = ToolCallContext::empty();
    // "domain_rules" with no action → OverlayRead → non-graph tool
    let args = serde_json::json!({});
    let _result = router.call_tool(&ctx, "domain_rules", &args);

    assert!(
        !router
            .project()
            .graph_runtime
            .state
            .graph_initialized
            .load(std::sync::atomic::Ordering::Acquire),
        "graph should still NOT be initialized after a non-graph tool call",
    );
}

#[test]
fn normalize_project_relative_path_accepts_include() {
    assert_eq!(
        normalize_project_relative_path("include"),
        Some("include".into())
    );
}

#[test]
fn normalize_project_relative_path_strips_dot_slash() {
    assert_eq!(
        normalize_project_relative_path("./src/include"),
        Some("src/include".into())
    );
}

#[test]
fn normalize_project_relative_path_converts_backslash() {
    assert_eq!(
        normalize_project_relative_path("src\\include"),
        Some("src/include".into())
    );
}

#[test]
fn normalize_project_relative_path_rejects_escape() {
    assert_eq!(normalize_project_relative_path("../outside"), None);
}

#[test]
fn normalize_project_relative_path_collapses_double_dot_within() {
    assert_eq!(
        normalize_project_relative_path("a/b/../c"),
        Some("a/c".into())
    );
}

#[test]
fn warnings_to_trace_diagnostics_converts() {
    let diags = warnings_to_trace_diagnostics(vec!["test error".into()], "test_code");
    assert!(!diags.is_empty());
    assert_eq!(diags[0].code, Some("test_code".into()));
    assert_eq!(diags[0].message, "test error");
}

#[test]
fn add_json_warnings_empty_is_noop() {
    let mut val = serde_json::json!({});
    add_json_warnings(&mut val, vec![], vec![]);
    assert!(val.get("warnings").is_none());
}

#[test]
fn add_json_warnings_merges() {
    let mut val = serde_json::json!({});
    add_json_warnings(&mut val, vec!["r1".into()], vec!["l1".into()]);
    let warns = val["warnings"].as_array().unwrap();
    assert_eq!(warns.len(), 2);
}

#[test]
fn status_reports_manifest_catalog_tier_from_fresh_layers() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    store
        .upsert_file_extraction_state(
            &file_id,
            "manifest",
            "hash1",
            "complete",
            atlas_engine::structs::FactCoverage::default(),
        )
        .unwrap();

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let (resp_str, is_error) = router.handle_status();
    assert!(!is_error, "status failed: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["index"]["catalog_tier"].as_str(), Some("manifest"));
    assert_eq!(resp["index"]["active_extraction_jobs"].as_u64(), Some(0));
}

#[test]
fn jobs_lists_active_extraction_jobs() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    store
        .claim_file_extraction_job(&file_id, "structural", Some("test"), None, Some(30_000))
        .unwrap();

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let (resp_str, is_error) = router.handle_jobs();
    assert!(!is_error, "jobs failed: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let jobs = resp["active_jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["layer"].as_str(), Some("structural"));
}

#[test]
fn tasks_query_status_uses_snapshot_job_tracker() {
    use atlas_engine::focus::job_tracker::JobTracker;

    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let tracker = Arc::new(JobTracker::new());
    router.store_query_snapshot(QuerySnapshot {
        query_id: "q_pending".into(),
        tool_name: "explore".into(),
        tool_args: serde_json::json!({"symbol": "target"}),
        focus_result: Some(atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: None,
            gaps: vec![],
            pending_closure_ids: vec!["cl_pending".into()],
            pending_extraction_job_ids: vec![],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: Some(Arc::clone(&tracker)),
        }),
        created_at: std::time::Instant::now(),
        status: crate::tools::query_snapshot::QueryStatus::Retryable,
    });

    let (pending, pending_err) = router.handle_tasks(&serde_json::json!({"query_id": "q_pending"}));
    assert!(!pending_err);
    let pending: serde_json::Value = serde_json::from_str(&pending).unwrap();
    assert_eq!(pending["query"]["status"], "refining");
    assert_eq!(pending["query"]["pending_jobs"], 1);
    assert_eq!(pending["query"]["retry_after_ms"], 5000);

    tracker.mark_done("cl_pending");
    let (ready, ready_err) = router.handle_tasks(&serde_json::json!({"query_id": "q_pending"}));
    assert!(!ready_err);
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(ready["query"]["status"], "ready");
    assert_eq!(ready["query"]["pending_jobs"], 0);
    assert!(ready["query"].get("retry_after_ms").is_none());
}

#[test]
fn tasks_query_status_tracks_raw_extraction_pending_jobs() {
    let store = test_store();
    let file_id = register_test_file(&store, "pending.ts");
    store
        .claim_file_extraction_job(&file_id, "structural", Some("q_raw"), None, Some(30_000))
        .unwrap();
    let job_id = store
        .find_active_file_extraction_job(&file_id, "structural")
        .unwrap()
        .expect("claimed job should be active")
        .job_id;

    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    router.store_query_snapshot(QuerySnapshot {
        query_id: "q_raw".into(),
        tool_name: "explore".into(),
        tool_args: serde_json::json!({"symbol": "target"}),
        focus_result: Some(atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: None,
            gaps: vec![],
            pending_closure_ids: vec![],
            pending_extraction_job_ids: vec![job_id.clone()],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: None,
        }),
        created_at: std::time::Instant::now(),
        status: crate::tools::query_snapshot::QueryStatus::Retryable,
    });

    let (pending, pending_err) = router.handle_tasks(&serde_json::json!({"query_id": "q_raw"}));
    assert!(!pending_err);
    let pending: serde_json::Value = serde_json::from_str(&pending).unwrap();
    assert_eq!(pending["query"]["status"], "refining");
    assert_eq!(pending["query"]["pending_jobs"], 1);
    assert_eq!(pending["query"]["retry_after_ms"], 5000);

    store.complete_extraction_job(&job_id).unwrap();
    let (ready, ready_err) = router.handle_tasks(&serde_json::json!({"query_id": "q_raw"}));
    assert!(!ready_err);
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(ready["query"]["status"], "ready");
    assert_eq!(ready["query"]["pending_jobs"], 0);
    assert!(
        ready["query"].get("retry_after_ms").is_none(),
        "completed raw extraction job must stop query retry: {ready}"
    );
}

#[test]
fn tasks_query_atlas_jobs_do_not_infer_completion_from_missing_rows() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

    let (resp, is_error) = router.handle_tasks(&serde_json::json!({"query_id": "q_missing"}));

    assert!(!is_error, "tasks failed: {resp}");
    let resp: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(
        resp["atlas_jobs"]["message"].as_str(),
        Some("no active extraction jobs")
    );
    assert_ne!(
        resp["atlas_jobs"]["message"].as_str(),
        Some("all jobs complete"),
        "raw extraction_jobs rows are not durable completion evidence"
    );
}

#[test]
fn symbol_not_found_without_candidates_is_terminal() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

    let (resp, is_error) = router.retryable_symbol_not_found_response(
        "symbol",
        &serde_json::json!({"symbol": "missing_func"}),
        "missing_func",
        Vec::new(),
        None,
    );

    assert!(
        !is_error,
        "bounded not-found should not be an error: {resp}"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(resp["status"], "unresolved");
    assert!(resp["analysis"].get("retry_after_ms").is_none(), "{resp}");
    assert!(
        resp.get("retry_after_ms").is_none(),
        "retry guidance belongs only under analysis: {resp}"
    );
    assert!(
        resp["gaps"].as_array().is_some_and(|gaps| gaps
            .iter()
            .any(|gap| gap["reason"] == "symbol_not_materialized")),
        "terminal unresolved response must expose its gap: {resp}"
    );
    assert!(resp.get("query_id").is_some(), "missing query_id: {resp}");
    assert!(resp.get("partial_result").is_none());
    assert!(resp.get("background_refinement").is_none());
    assert!(resp.get("work").is_none());
}

// ── Regression: include_roots validation produces diagnostics ────

#[test]
fn trace_point_invalid_include_roots_returns_diagnostics() {
    let store = test_store();
    register_test_file(&store, "test.ts");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

    let args = serde_json::json!({
        "file_path": "test.ts",
        "line": 1,
        "column": 1,
        "include_roots": ["/absolute/rejected"]
    });

    let (resp_str, _is_error) = router.handle_trace_point(&ToolCallContext::empty(), &args);
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let diags = resp["diagnostics"].as_array().unwrap();
    assert!(
        !diags.is_empty(),
        "Expected diagnostics for invalid include_roots"
    );
    let codes: Vec<&str> = diags.iter().filter_map(|d| d["code"].as_str()).collect();
    assert!(
        codes.contains(&"include_roots_warning"),
        "Expected include_roots_warning code, got: {codes:?}"
    );
}

#[test]
fn trace_variable_invalid_include_roots_returns_diagnostics() {
    let store = test_store();
    register_test_file(&store, "test.ts");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

    let args = serde_json::json!({
        "file_path": "test.ts",
        "line": 1,
        "column": 1,
        "max_depth": 5,
        "include_roots": ["/absolute/rejected"]
    });

    let (resp_str, _) = router.handle_trace_variable(&args);
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let diags = resp["diagnostics"].as_array();
    assert!(diags.is_some(), "Expected diagnostics");
    let codes: Vec<&str> = diags
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"include_roots_warning"),
        "Expected include_roots_warning, got: {codes:?}"
    );
}

// ── Regression: symbol/context with invalid include_roots → warnings ──

#[test]
fn symbol_existing_invalid_include_roots_returns_warning() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    insert_test_symbol(&store, file_id, "test_func");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "symbol": "test_func.test_func",
        "include_roots": ["/absolute/rejected"]
    });

    let (resp_str, is_error) = router.handle_symbol(&ToolCallContext::empty(), &args);
    assert!(!is_error, "Expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let warns = resp["warnings"].as_array();
    assert!(warns.is_some(), "Expected 'warnings' field in: {resp_str}");
    assert!(
        !warns.unwrap().is_empty(),
        "Expected non-empty warnings in: {resp_str}"
    );
}

#[test]
fn symbol_detail_returns_stored_signature() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    insert_test_symbol_with_signature(
        &store,
        file_id,
        "test_func",
        Some("async (arg: string): void"),
        true,
        true,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    assert!(
        !router
            .project()
            .graph_runtime
            .state
            .graph_initialized
            .load(Ordering::Acquire)
    );

    let args = serde_json::json!({
        "symbol": "test_func.test_func",
        "view": "detail"
    });

    let (resp_str, is_error) = router.handle_symbol(&ToolCallContext::empty(), &args);
    assert!(!is_error, "Expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["signature"].as_str(),
        Some("async (arg: string): void"),
        "symbol detail must pass through the stored SymbolDef.signature"
    );
    assert_eq!(resp["exported"], true);
    assert_eq!(resp["async"], true);
    assert!(resp.get("callers").is_none());
    assert!(resp.get("callees").is_none());
    assert!(
        !router
            .project()
            .graph_runtime
            .state
            .graph_initialized
            .load(Ordering::Acquire),
        "symbol detail must not initialize the graph"
    );
}

#[test]
fn context_existing_invalid_include_roots_returns_warning() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    insert_test_symbol(&store, file_id, "test_func");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "symbol": "test_func.test_func",
        "include_roots": ["/absolute/rejected"]
    });

    let (resp_str, is_error) = router.handle_context(&ToolCallContext::empty(), &args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let warns = resp["warnings"].as_array();
    assert!(warns.is_some(), "Expected 'warnings' field in: {resp_str}");
    assert!(
        !warns.unwrap().is_empty(),
        "Expected non-empty warnings in: {resp_str}"
    );
}

#[test]
fn context_include_file_peers_false_produces_empty_file_peers() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    insert_test_symbol(&store, file_id, "test_func");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "symbol": "test_func.test_func",
        "includeFilePeers": false,
    });

    let (resp_str, is_error) = router.handle_context(&ToolCallContext::empty(), &args);
    assert!(!is_error, "Expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let file_peers = resp["file_peers"].as_array();
    assert!(
        file_peers.is_some(),
        "Expected 'file_peers' field in: {resp_str}"
    );
    assert!(
        file_peers.unwrap().is_empty(),
        "Expected empty file_peers when includeFilePeers=false, got: {resp_str}"
    );
}

// ── file_dependencies tests ──────────────────────────────────────────

/// Helper: insert a symbol edge between two symbols.
fn insert_test_edge(store: &Store, source: SymbolId, target: SymbolId) {
    use atlas_engine::Confidence;
    use atlas_engine::EdgeKind;
    use atlas_engine::Provenance;
    let edge = atlas_engine::RawEdge::new(
        atlas_engine::EdgeId::generate(&source, &target, "calls", None, "tree_sitter"),
        source,
        target,
        EdgeKind::Calls,
        Confidence::new(1.0),
        Provenance::TreeSitter,
    );
    store.insert_edges(&[edge]).unwrap();
}

/// Helper: insert an import from one file to another.
fn insert_test_import(store: &Store, from_file: FileId, to_path: &str, imported_name: &str) {
    use atlas_engine::ImportKind;
    let range = atlas_engine::TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 11,
    };
    let import = atlas_engine::ImportDef {
        id: atlas_engine::ImportId::generate(&from_file, "import", to_path, Some(imported_name), 0),
        file_id: from_file,
        kind: ImportKind::Import,
        module: to_path.to_string(),
        imported_name: imported_name.to_string(),
        local_name: None,
        is_wildcard: false,
        is_relative: false,
        range,
        alias: None,
    };
    store.insert_imports(&[import]).unwrap();
}

fn assert_manifest_analysis(resp: &serde_json::Value, resp_str: &str) {
    assert!(
        resp.get("analysis_contract").is_none(),
        "legacy contract field must not be public: {resp_str}"
    );
    assert_eq!(resp["analysis"]["scope"].as_str(), Some("local"));
    assert_eq!(resp["analysis"]["basis"], serde_json::json!(["manifest"]));
    assert!(resp["analysis"].get("missing").is_none());
}

#[test]
fn manifest_incoming_returns_correct_deps() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");

    // File B imports from A, and has a call edge to A's symbol
    let sym_a = SymbolId::generate(&file_a, "typescript", "foo", "function", None);
    let sym_b = SymbolId::generate(&file_b, "typescript", "bar", "function", None);
    insert_test_symbol(&store, file_a, "foo");
    insert_test_symbol(&store, file_b, "bar");

    // Edge: B's bar calls A's foo → B depends on A
    insert_test_edge(&store, sym_b, sym_a);

    // Import: B imports from a.ts
    insert_test_import(&store, file_b, "a.ts", "foo");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "incoming",
        "analysis": "manifest",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_manifest_analysis(&resp, &resp_str);

    // Should have at least the import-based dependent (b.ts)
    let deps = resp["dependents"].as_array().unwrap();
    let dep_files: Vec<&str> = deps.iter().filter_map(|d| d["file"].as_str()).collect();
    assert!(
        dep_files.contains(&"b.ts"),
        "Expected b.ts in dependents, got: {dep_files:?}"
    );

    // The edge-based dependent should also be there (from symbol_edges)
    // Both import and edge point to b.ts, deduplication should result in one entry
    assert!(
        !dep_files.is_empty(),
        "Expected at least one dependent, got: {resp_str}"
    );
}

#[test]
fn focus_manifest_incoming_converges_to_importers_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("VideoComponent.ets"),
        "@Component\nexport struct VideoComponent {}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("SampleComponent.ets"),
        "import { VideoComponent } from './VideoComponent';\n@Component\nstruct SampleComponent {}\n",
    )
    .unwrap();

    let store = test_store();
    for path in ["src/VideoComponent.ets", "src/SampleComponent.ets"] {
        let file_id = FileId::generate(path);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id,
                path: path.into(),
                language: atlas_engine::Language::ArkTS,
                content_hash: "cold".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
    }
    let router = ToolRouter::new_empty(store, root.path().to_path_buf());

    let args = serde_json::json!({
        "file_path": "src/VideoComponent.ets",
        "direction": "incoming",
        "analysis": "manifest",
    });
    let started = std::time::Instant::now();
    let (body, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "{body}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "dependency query must enqueue refinement instead of waiting for structural closure"
    );

    let mut response: serde_json::Value = serde_json::from_str(&body).unwrap();
    let query_id = response["query_id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        if response["analysis"].get("retry_after_ms").is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (body, is_error) =
            router.handle_resume_query(&serde_json::json!({"query_id": query_id}));
        assert!(!is_error, "{body}");
        response = serde_json::from_str(&body).unwrap();
    }

    assert!(
        response["analysis"].get("retry_after_ms").is_none(),
        "dependency refinement must converge: {response}"
    );
    let importer_id = FileId::generate("src/SampleComponent.ets");
    let imports = router
        .project()
        .store
        .find_imports_by_file(&importer_id)
        .unwrap();
    assert!(!imports.is_empty(), "importer facts were not materialized");
    let dependents = response["dependents"].as_array().unwrap();
    assert!(
        dependents
            .iter()
            .any(|dependent| dependent["file"] == "src/SampleComponent.ets"),
        "incoming dependency must be visible after resume: {response}"
    );
}

#[test]
fn manifest_outgoing_returns_correct_deps() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");

    let sym_a = SymbolId::generate(&file_a, "typescript", "foo", "function", None);
    let sym_b = SymbolId::generate(&file_b, "typescript", "bar", "function", None);
    insert_test_symbol(&store, file_a, "foo");
    insert_test_symbol(&store, file_b, "bar");

    // Edge: A's foo calls B's bar → A depends on B
    insert_test_edge(&store, sym_a, sym_b);

    // Import: A imports from b.ts
    insert_test_import(&store, file_a, "b.ts", "bar");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "outgoing",
        "analysis": "manifest",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_manifest_analysis(&resp, &resp_str);

    let deps = resp["dependencies"].as_array().unwrap();
    let dep_modules: Vec<&str> = deps.iter().filter_map(|d| d["module"].as_str()).collect();
    assert!(
        dep_modules.contains(&"b.ts"),
        "Expected b.ts in dependencies, got: {dep_modules:?}"
    );
}

#[test]
fn manifest_both_returns_analysis() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");

    let sym_a = SymbolId::generate(&file_a, "typescript", "foo", "function", None);
    let sym_b = SymbolId::generate(&file_b, "typescript", "bar", "function", None);
    insert_test_symbol(&store, file_a, "foo");
    insert_test_symbol(&store, file_b, "bar");

    insert_test_edge(&store, sym_b, sym_a); // B → A: incoming
    insert_test_edge(&store, sym_a, sym_b); // A → B: outgoing
    insert_test_import(&store, file_b, "a.ts", "foo");
    insert_test_import(&store, file_a, "b.ts", "bar");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "both",
        "analysis": "manifest",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_manifest_analysis(&resp, &resp_str);
}

#[test]
fn structural_returns_analysis() {
    let store = test_store();
    let _file_a = register_test_file(&store, "a.ts");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "incoming",
        "analysis": "structural",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    // analysis block must be present (unified envelope)
    let analysis = &resp["analysis"];
    assert!(
        analysis.get("scope").is_some(),
        "analysis block missing scope field: {resp_str}"
    );
    assert!(
        analysis.get("summary").is_some(),
        "analysis block missing summary field: {resp_str}"
    );
}

#[test]
fn manifest_analysis_reports_ready() {
    let store = test_store();
    let _file_a = register_test_file(&store, "a.ts");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "incoming",
        "analysis": "manifest",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_manifest_analysis(&resp, &resp_str);
}

#[test]
fn manifest_default_when_analysis_omitted() {
    let store = test_store();
    let _file_a = register_test_file(&store, "a.ts");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    // Omit analysis parameter — should default to manifest
    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "incoming",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_manifest_analysis(&resp, &resp_str);
}

#[test]
fn unknown_analysis_mode_returns_error() {
    let store = test_store();
    let _file_a = register_test_file(&store, "a.ts");

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "incoming",
        "analysis": "invalid",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(
        is_error,
        "Expected error for unknown analysis mode, got: {resp_str}"
    );
    assert!(
        resp_str.contains("Unknown analysis mode"),
        "Expected error message, got: {resp_str}"
    );
}

#[test]
fn manifest_edge_dependencies_via_calls() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");
    let file_c = register_test_file(&store, "c.ts");

    let sym_a = SymbolId::generate(&file_a, "typescript", "target", "function", None);
    let sym_b = SymbolId::generate(&file_b, "typescript", "caller_b", "function", None);
    let sym_c = SymbolId::generate(&file_c, "typescript", "caller_c", "function", None);
    insert_test_symbol(&store, file_a, "target");
    insert_test_symbol(&store, file_b, "caller_b");
    insert_test_symbol(&store, file_c, "caller_c");

    // Both B and C call A
    insert_test_edge(&store, sym_b, sym_a);
    insert_test_edge(&store, sym_c, sym_a);

    // No imports — edge-based deps only
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "file_path": "a.ts",
        "direction": "incoming",
        "analysis": "manifest",
    });
    let (resp_str, is_error) = router.handle_file_dependencies(&args);
    assert!(!is_error, "Expected success, got: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let deps = resp["dependents"].as_array().unwrap();
    let dep_files: Vec<&str> = deps.iter().filter_map(|d| d["file"].as_str()).collect();
    assert!(
        dep_files.contains(&"b.ts"),
        "Expected edge-based dependent b.ts, got: {dep_files:?}"
    );
    assert!(
        dep_files.contains(&"c.ts"),
        "Expected edge-based dependent c.ts, got: {dep_files:?}"
    );
}

// ── ToolCallContext tests ────────────────────────────────────────────

#[test]
fn tool_call_context_is_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<ToolCallContext>();
    assert_sync::<ToolCallContext>();
}

#[test]
fn tool_call_context_empty_does_not_panic_on_send_progress() {
    let ctx = ToolCallContext::empty();
    // Should be a no-op — no panic when no progress_sender is set.
    ctx.send_progress(0.5, "test message");
    ctx.send_progress(1.0, "final message");
}

#[test]
fn tool_call_context_with_progress_sender_forwards() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProgressReport>();
    let ctx = ToolCallContext::with_progress_sender(tx);
    ctx.send_progress(0.5, "halfway");

    // Drop ctx to close the sender, then drain
    drop(ctx);
    let reports: Vec<ProgressReport> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(reports.len(), 1, "Expected exactly 1 progress report");
    assert_eq!(reports[0].0, 0.5);
    assert_eq!(reports[0].2.as_deref(), Some("halfway"));
}

// ── Test helpers ───────────────────────────────────────────────────

/// Insert a minimal symbol with a caller-controlled qualified name.
fn insert_test_symbol_with_qname(
    store: &Store,
    file_id: FileId,
    simple_name: &str,
    qualified_name: &str,
    kind: atlas_engine::SymbolKind,
) {
    let range = atlas_engine::TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 11,
    };
    let sym = atlas_engine::SymbolDef {
        id: SymbolId::generate(&file_id, "typescript", simple_name, kind.as_str(), None),
        kind,
        name: simple_name.into(),
        qualified_name: qualified_name.into(),
        symbol_path: vec![simple_name.into()],
        file_id,
        language: atlas_engine::Language::TypeScript,
        range,
        name_range: range,
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".into(),
    };
    store.insert_symbols(&[sym]).unwrap();
}

// ── Trace E2E tests ─────────────────────────────────────────────────

/// Helper: insert a symbol with a custom qname and return its SymbolId
/// for edge construction.
fn insert_trace_test_symbol(
    store: &Store,
    file_id: FileId,
    simple_name: &str,
    qualified_name: &str,
    kind: atlas_engine::SymbolKind,
) -> SymbolId {
    let id = SymbolId::generate(&file_id, "typescript", simple_name, kind.as_str(), None);
    // Use the existing insert_test_symbol_with_qname to insert the symbol.
    // Reconstruct the same SymbolId to ensure edges refer to the correct id.
    insert_test_symbol_with_qname(store, file_id, simple_name, qualified_name, kind);
    id
}

// ── A. trace_callers E2E ────────────────────────────────────────────

#[test]
fn trace_callers_with_edge_returns_path() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");

    let caller_id = insert_trace_test_symbol(
        &store,
        file_a,
        "caller_func",
        "caller_func",
        atlas_engine::SymbolKind::Function,
    );
    let callee_id = insert_trace_test_symbol(
        &store,
        file_b,
        "callee_func",
        "callee_func",
        atlas_engine::SymbolKind::Function,
    );
    insert_test_edge(&store, caller_id, callee_id);

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({"symbol": "callee_func"});
    let (resp_str, is_error) = router.handle_trace_caller_path(&args);

    assert!(!is_error, "Expected no error, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["kind"].as_str(),
        Some("trace_callers"),
        "Expected kind=trace_callers, got: {resp_str}"
    );
    // With an edge inserted, the result should have callers/path data.
    let ok = resp["ok"].as_bool().unwrap_or(false);
    assert!(ok, "Expected ok=true with callers data, got: {resp_str}");
}

#[test]
fn trace_callers_ambiguous_symbol() {
    // BestEffortSingle picks the first candidate when multiple symbols
    // share the same qualified name.  The test verifies that a trace
    // succeeds rather than returning an ambiguous error.
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");
    let file_c = register_test_file(&store, "c.ts");

    insert_test_symbol_with_qname(
        &store,
        file_a,
        "turn",
        "turn",
        atlas_engine::SymbolKind::Function,
    );
    insert_test_symbol_with_qname(
        &store,
        file_b,
        "turn",
        "turn",
        atlas_engine::SymbolKind::Variable,
    );
    insert_test_symbol_with_qname(
        &store,
        file_c,
        "turn",
        "turn",
        atlas_engine::SymbolKind::Method,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({"symbol": "turn"});
    let (resp_str, is_error) = router.handle_trace_caller_path(&args);

    assert!(
        !is_error,
        "BestEffortSingle should pick a candidate, got: {resp_str}"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["kind"].as_str(),
        Some("trace_callers"),
        "Expected trace_callers response, got: {resp_str}"
    );
}

// ── B. trace_forward E2E ────────────────────────────────────────────

#[test]
fn trace_forward_with_edge_returns_path() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");

    let from_id = insert_trace_test_symbol(
        &store,
        file_a,
        "from_func",
        "from_func",
        atlas_engine::SymbolKind::Function,
    );
    let to_id = insert_trace_test_symbol(
        &store,
        file_b,
        "to_func",
        "to_func",
        atlas_engine::SymbolKind::Function,
    );
    insert_test_edge(&store, from_id, to_id);

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({"from": "from_func", "to": "to_func"});
    let (resp_str, is_error) = router.handle_trace_forward(&args);

    assert!(!is_error, "Expected no error, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["kind"].as_str(),
        Some("trace_forward"),
        "Expected kind=trace_forward, got: {resp_str}"
    );
    let ok = resp["ok"].as_bool().unwrap_or(false);
    assert!(ok, "Expected ok=true with path data, got: {resp_str}");
}

#[test]
fn trace_forward_ambiguous_to_path_aware() {
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");
    let file_c = register_test_file(&store, "c.ts");

    let from_id = insert_trace_test_symbol(
        &store,
        file_a,
        "from_func",
        "from_func",
        atlas_engine::SymbolKind::Function,
    );
    // Two "to_func" symbols — only one reachable from from_func
    let reachable_id = insert_trace_test_symbol(
        &store,
        file_b,
        "to_func",
        "to_func",
        atlas_engine::SymbolKind::Function,
    );
    insert_trace_test_symbol(
        &store,
        file_c,
        "to_func",
        "to_func",
        atlas_engine::SymbolKind::Function,
    );
    insert_test_edge(&store, from_id, reachable_id);

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({"from": "from_func", "to": "to_func"});
    let (resp_str, is_error) = router.handle_trace_forward(&args);

    assert!(
        !is_error,
        "Path-aware disambiguation should succeed, got: {resp_str}"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["kind"].as_str(),
        Some("trace_forward"),
        "Expected kind=trace_forward, got: {resp_str}"
    );
}

#[test]
fn trace_forward_ambiguous_to_no_reachable() {
    // BestEffortSingle picks the first 'to' candidate even without
    // path-aware disambiguation.  The trace_forward call succeeds
    // and returns no_path_found.
    let store = test_store();
    let file_a = register_test_file(&store, "a.ts");
    let file_b = register_test_file(&store, "b.ts");
    let file_c = register_test_file(&store, "c.ts");

    insert_trace_test_symbol(
        &store,
        file_a,
        "from_func",
        "from_func",
        atlas_engine::SymbolKind::Function,
    );
    // Two "to_func" symbols — neither reachable
    insert_trace_test_symbol(
        &store,
        file_b,
        "to_func",
        "to_func",
        atlas_engine::SymbolKind::Function,
    );
    insert_trace_test_symbol(
        &store,
        file_c,
        "to_func",
        "to_func",
        atlas_engine::SymbolKind::Function,
    );
    // No edge between them

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({"from": "from_func", "to": "to_func"});
    let (resp_str, is_error) = router.handle_trace_forward(&args);

    assert!(
        !is_error,
        "BestEffortSingle should pick candidate even without path, got: {resp_str}"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(
        resp["kind"].as_str(),
        Some("trace_forward"),
        "Expected trace_forward response"
    );
    // No path should be found between the two
    let diags = resp["diagnostics"].as_array().unwrap();
    let codes: Vec<&str> = diags.iter().filter_map(|d| d["code"].as_str()).collect();
    assert!(
        codes.contains(&"no_path_found"),
        "Expected no_path_found code, got: {codes:?}"
    );
}

// ── C. trace_variable envelope metadata ────────────────────────────

#[test]
fn trace_variable_has_analysis_or_query_id() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    insert_test_symbol_with_qname(
        &store,
        file_id,
        "main",
        "main",
        atlas_engine::SymbolKind::Function,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({
        "file_path": "test.ts",
        "line": 1,
        "column": 1,
    });
    let (resp_str, _is_error) = router.handle_trace_variable(&args);

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    // The response must include kind
    assert_eq!(
        resp["kind"].as_str(),
        Some("trace_variable"),
        "Expected kind=trace_variable, got: {resp_str}"
    );
    // Check for the analysis block as evidence the
    // envelope injected its metadata
    let has_analysis = resp.get("analysis").is_some();
    let has_query_id = resp.get("query_id").is_some();
    assert!(
        has_analysis || has_query_id,
        "Expected analysis or query_id field, got: {resp_str}"
    );
}

// ── E. Hex SymbolId resolution in callers ──────────────────────────

#[test]
fn trace_callers_hex_symbol_accepted() {
    // Hex strings are no longer auto-detected — they are treated as
    // qualified names. A hex-looking string won't match any symbol.
    // In focus mode, this returns a retryable unresolved result instead
    // of a hard error.
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    let sym_name = "my_func";
    let kind = atlas_engine::SymbolKind::Function;
    let sym_id = SymbolId::generate(&file_id, "typescript", sym_name, kind.as_str(), None);
    insert_test_symbol_with_qname(&store, file_id, sym_name, "my_func", kind);
    let hex_id = sym_id.to_hex();

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let args = serde_json::json!({"symbol": hex_id});
    let (resp_str, is_error) = router.handle_trace_caller_path(&args);

    assert!(
        resp_str.contains("not found")
            || resp_str.contains("not available")
            || resp_str.contains("unresolved")
            || is_error,
        "Hex string should not resolve as SymbolId: {resp_str}"
    );
}

#[test]
fn trace_tool_has_per_kind_descriptions() {
    let tools = make_trace_tools();
    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool.name, "trace");

    let props = tool
        .input_schema
        .properties
        .as_ref()
        .expect("should have properties");
    let kind = props.get("kind").expect("should have kind property");

    // Verify oneOf is present with 4 variants
    let one_of = kind.get("oneOf").expect("kind should have oneOf");
    let variants = one_of.as_array().expect("oneOf should be array");
    assert_eq!(variants.len(), 4);

    let descriptions: Vec<&str> = variants
        .iter()
        .map(|v| v.get("description").and_then(|d| d.as_str()).unwrap_or(""))
        .collect();

    assert!(
        descriptions[0].contains("position"),
        "point description missing: {:?}",
        descriptions[0]
    );
    assert!(
        descriptions[1].contains("dataflow"),
        "variable description should mention dataflow"
    );
    assert!(
        descriptions[2].contains("call-graph"),
        "forward description should mention call-graph"
    );
    assert!(
        descriptions[3].contains("call-graph"),
        "callers description should mention call-graph"
    );
}

// ── Position-based symbol lookup tests ───────────────────────────

/// Helper: insert a symbol with a specific source range.
fn insert_symbol_with_range(
    store: &Store,
    file_id: FileId,
    name: &str,
    kind: atlas_engine::SymbolKind,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
) -> atlas_engine::SymbolId {
    let range = atlas_engine::TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line,
        start_column: start_col,
        end_line,
        end_column: end_col,
    };
    let id = atlas_engine::SymbolId::generate(&file_id, "typescript", name, "function", None);
    let sym = atlas_engine::SymbolDef {
        id,
        kind,
        name: name.into(),
        qualified_name: format!("{name}.{name}"),
        symbol_path: vec![name.into()],
        file_id,
        language: atlas_engine::Language::TypeScript,
        range,
        name_range: range,
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".into(),
    };
    store.insert_symbols(&[sym]).unwrap();
    id
}

#[test]
fn is_definition_kind_all_are_definitions() {
    // All currently defined SymbolKind variants are definitions.
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Function));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Class));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Struct));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Interface));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Enum));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::TypeAlias));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Variable));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Field));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Method));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Module));
    assert!(is_definition_kind(&atlas_engine::SymbolKind::Parameter));
}

#[test]
fn position_lookup_picks_innermost_symbol() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/test.ts");

    // Outer: function at lines 1-10 (0-based: 0..9), cols 0-80
    insert_symbol_with_range(
        &store,
        file_id,
        "outer",
        atlas_engine::SymbolKind::Function,
        0,
        0,
        9,
        80,
    );
    // Inner: function at lines 3-5 (0-based: 2..4), cols 0-40
    insert_symbol_with_range(
        &store,
        file_id,
        "inner",
        atlas_engine::SymbolKind::Function,
        2,
        0,
        4,
        40,
    );

    let symbols = store.find_symbols_by_file(&file_id).unwrap();
    assert_eq!(symbols.len(), 2);

    // Position at line 4 (1-based) → 0-based line 3 should match inner
    let line_1based: u32 = 4;
    let target_line_0based = line_1based - 1;
    let target_col_0based: u32 = 1; // column 1 (ignored for line-only check)
    let inner_symbols: Vec<_> = symbols
        .iter()
        .filter(|s| {
            is_definition_kind(&s.kind)
                && s.range.start_line <= target_line_0based
                && target_line_0based <= s.range.end_line
                && s.range.start_column <= target_col_0based
                && target_col_0based <= s.range.end_column
        })
        .collect();
    assert_eq!(inner_symbols.len(), 2, "both outer and inner cover line 4");

    // Pick smallest range: (line_span, column_span)
    let mut sorted: Vec<_> = inner_symbols.iter().collect();
    sorted.sort_by_key(|s| {
        (s.range.end_line - s.range.start_line) * 1_000_000
            + (s.range.end_column - s.range.start_column)
    });
    assert_eq!(
        sorted[0].name, "inner",
        "Should pick innermost (smallest range) symbol"
    );
}

#[test]
fn position_lookup_no_candidates_returns_empty() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/empty.ts");

    // Insert symbol at lines 1-3
    insert_symbol_with_range(
        &store,
        file_id,
        "func",
        atlas_engine::SymbolKind::Function,
        0,
        0,
        2,
        10,
    );

    let symbols = store.find_symbols_by_file(&file_id).unwrap();
    assert_eq!(symbols.len(), 1);

    // Query line 10 (1-based) → no symbol should cover it
    let line_1based: u32 = 10;
    let target_line = line_1based - 1;
    let matches: Vec<_> = symbols
        .iter()
        .filter(|s| {
            is_definition_kind(&s.kind)
                && s.range.start_line <= target_line
                && target_line <= s.range.end_line
        })
        .collect();
    assert!(matches.is_empty(), "no symbol should cover line 10");
}

#[test]
fn position_lookup_respects_column_filter() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/coltest.ts");

    // Two symbols on the same line span but different columns
    insert_symbol_with_range(
        &store,
        file_id,
        "left",
        atlas_engine::SymbolKind::Variable,
        2,
        0,
        2,
        20, // line 3 (0-based 2), cols 0-20
    );
    insert_symbol_with_range(
        &store,
        file_id,
        "right",
        atlas_engine::SymbolKind::Variable,
        2,
        25,
        2,
        45, // line 3 (0-based 2), cols 25-45
    );

    let symbols = store.find_symbols_by_file(&file_id).unwrap();
    let target_line = 2; // 0-based line 2
    let target_col: u32 = 30; // col 30, should match "right" only

    let matches: Vec<_> = symbols
        .iter()
        .filter(|s| {
            is_definition_kind(&s.kind)
                && s.range.start_line <= target_line
                && target_line <= s.range.end_line
                && s.range.start_column <= target_col
                && target_col <= s.range.end_column
        })
        .collect();
    assert_eq!(matches.len(), 1, "only 'right' should match column 30");
    assert_eq!(matches[0].name, "right");
}

// ── merge_edge_deps tests ─────────────────────────────────────────────

#[test]
fn test_merge_edge_deps_empty() {
    let mut value = serde_json::json!({"dependents": []});
    let edge_deps = serde_json::json!([]);
    merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
    assert_eq!(value["dependents"].as_array().unwrap().len(), 0);
}

#[test]
fn test_merge_edge_deps_into_existing() {
    let mut value = serde_json::json!({
        "dependents": [{"file": "a.ts"}],
        "total_dependents": 1,
    });
    let edge_deps = serde_json::json!([
        {"file": "b.ts"},
        {"file": "c.ts"},
    ]);
    merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
    assert_eq!(value["dependents"].as_array().unwrap().len(), 3);
    assert_eq!(value["total_dependents"].as_u64().unwrap(), 3);
}

#[test]
fn test_merge_edge_deps_into_empty() {
    let mut value = serde_json::json!({"dependents": []});
    let edge_deps = serde_json::json!([{"file": "b.ts"}]);
    merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
    assert_eq!(value["dependents"].as_array().unwrap().len(), 1);
}

#[test]
fn test_merge_edge_deps_no_list_field() {
    let mut value = serde_json::json!({"other": 1});
    let edge_deps = serde_json::json!([{"file": "b.ts"}]);
    // Should not panic — gracefully skip
    merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
    assert_eq!(value["other"].as_u64().unwrap(), 1);
}

// ── handle_symbol_by_position view dispatch tests ───────────────────────

#[test]
fn handle_symbol_by_position_with_detail_view() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/test.ts");
    // Use insert_symbol_with_range to place symbol at 0-based line 0 so that
    // user's 1-based line=1 matches it.
    insert_symbol_with_range(
        &store,
        file_id,
        "myFunc",
        atlas_engine::SymbolKind::Function,
        0,
        1,
        0,
        80, // (start_line, start_col, end_line, end_col) 0-based
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({
        "file_path": "src/test.ts",
        "line": 1,
        "view": "detail"
    });

    let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
    assert!(!is_error, "Expected no error, got: {resp_str}");
    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");
    // Detail view should have 'name' and 'qualified_name' fields
    assert!(
        resp.get("name").is_some(),
        "detail response should have 'name' field"
    );
    assert!(
        resp.get("qualified_name").is_some(),
        "detail response should have 'qualified_name' field"
    );
}

#[test]
fn handle_symbol_by_position_with_context_view() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/main.rs");
    insert_symbol_with_range(
        &store,
        file_id,
        "process",
        atlas_engine::SymbolKind::Function,
        0,
        1,
        0,
        80,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({
        "file_path": "src/main.rs",
        "line": 1,
        "view": "context"
    });

    let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
    assert!(!is_error, "Expected no error, got: {resp_str}");
    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");
    // Context view should have 'subject' not 'name' at top level
    assert!(
        resp.get("subject").is_some(),
        "context response should have 'subject' field, got keys: {:?}",
        resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
}

#[test]
fn handle_symbol_by_position_with_usages_view() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/lib.rs");
    insert_symbol_with_range(
        &store,
        file_id,
        "helper",
        atlas_engine::SymbolKind::Function,
        0,
        1,
        0,
        80,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({
        "file_path": "src/lib.rs",
        "line": 1,
        "view": "usages"
    });

    let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
    assert!(!is_error, "Expected no error, got: {resp_str}");
    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");
    // Usages view should have 'usages' array
    assert!(
        resp.get("usages").is_some(),
        "usages response should have 'usages' field, got keys: {:?}",
        resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    assert!(
        resp.get("total_usages").is_some(),
        "usages response should have 'total_usages' field"
    );
}

#[test]
fn handle_symbol_by_position_with_invalid_view() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/bad.rs");
    insert_symbol_with_range(
        &store,
        file_id,
        "func",
        atlas_engine::SymbolKind::Function,
        0,
        1,
        0,
        80,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({
        "file_path": "src/bad.rs",
        "line": 1,
        "view": "nonexistent"
    });

    let (_resp_str, is_error) = router.handle_symbol(&ctx, &args);
    assert!(is_error, "Should return error for unknown view");
}

// ── Structured selector + view=detail filtering tests ──────────────

#[test]
fn structured_selector_detail_resolves_uniquely() {
    let store = test_store();
    // Two symbols with SAME qualified_name but DIFFERENT files.
    let file_a = register_test_file(&store, "src/a.ts");
    let file_b = register_test_file(&store, "src/b.ts");
    insert_symbol_with_range(
        &store,
        file_a,
        "Helper",
        atlas_engine::SymbolKind::Function,
        0,
        1,
        0,
        80,
    );
    insert_symbol_with_range(
        &store,
        file_b,
        "Helper",
        atlas_engine::SymbolKind::Function,
        5,
        1,
        5,
        80,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let ctx = ToolCallContext::empty();
    // Structured selector with file_path to disambiguate.
    let args = serde_json::json!({
        "symbol": {
            "qualified_name": "Helper.Helper",
            "file_path": "src/a.ts"
        },
        "view": "detail"
    });

    let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
    assert!(
        !is_error,
        "Expected unique resolution, got error: {resp_str}"
    );
    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");
    assert!(
        resp.get("name").is_some(),
        "detail response should have 'name' field, got: {resp_str}"
    );
    // Verify the resolved file matches the selector.
    assert_eq!(
        resp.get("file").and_then(|v| v.as_str()).unwrap_or(""),
        "src/a.ts",
        "Should resolve to the file specified in the selector, got: {resp_str}"
    );
}

#[test]
fn plain_string_symbol_detail_still_works() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/main.ts");
    insert_symbol_with_range(
        &store,
        file_id,
        "SingleFunction",
        atlas_engine::SymbolKind::Function,
        0,
        1,
        0,
        80,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({
        "symbol": "SingleFunction.SingleFunction",
        "view": "detail"
    });

    let (resp_str, is_error) = router.handle_symbol(&ctx, &args);
    assert!(!is_error, "Expected no error, got: {resp_str}");
    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");
    assert!(
        resp.get("name").is_some(),
        "detail response should have 'name' field, got: {resp_str}"
    );
    assert_eq!(
        resp.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "SingleFunction",
        "Should resolve to SingleFunction, got: {resp_str}"
    );
}

// ── file_path diagnostic in ambiguous responses ──────────────────────

#[test]
fn invalid_file_path_in_selector_produces_diagnostic() {
    let store = test_store();
    let file_a = register_test_file(&store, "src/a.ts");
    let file_b = register_test_file(&store, "src/b.ts");

    // Two symbols with the same qualified name in different files → ambiguous.
    insert_test_symbol_with_qname(
        &store,
        file_a,
        "Foo",
        "Foo.Foo",
        atlas_engine::SymbolKind::Function,
    );
    insert_test_symbol_with_qname(
        &store,
        file_b,
        "Foo",
        "Foo.Foo",
        atlas_engine::SymbolKind::Function,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

    let args = serde_json::json!({
        "symbol": {
            "qualified_name": "Foo.Foo",
            "file_path": "src/nonexistent.ts"
        }
    });

    let (resp_str, is_error) = router.handle_symbol_detail(&args);
    assert!(
        is_error,
        "Expected error for ambiguous symbol, got: {resp_str}"
    );
    assert!(
        resp_str.contains("file_path 'src/nonexistent.ts' does not match any file"),
        "Expected file_path diagnostic in error, got: {resp_str}"
    );
}

#[test]
fn plain_string_ambiguous_no_false_diagnostic() {
    let store = test_store();
    let file_a = register_test_file(&store, "src/a.ts");
    let file_b = register_test_file(&store, "src/b.ts");

    // Two symbols with the same qualified name → ambiguous.
    insert_test_symbol_with_qname(
        &store,
        file_a,
        "Foo",
        "Foo.Foo",
        atlas_engine::SymbolKind::Function,
    );
    insert_test_symbol_with_qname(
        &store,
        file_b,
        "Foo",
        "Foo.Foo",
        atlas_engine::SymbolKind::Function,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

    let args = serde_json::json!({
        "symbol": "Foo.Foo"
    });

    let (resp_str, is_error) = router.handle_symbol_detail(&args);
    assert!(
        is_error,
        "Expected error for ambiguous symbol, got: {resp_str}"
    );
    assert!(
        !resp_str.contains("does not match any file"),
        "Should NOT contain file_path diagnostic for plain string input, got: {resp_str}"
    );
}

#[test]
fn valid_file_path_unambiguous_no_diagnostic_leak() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/main.ts");

    insert_test_symbol_with_qname(
        &store,
        file_id,
        "MyFunc",
        "MyFunc.MyFunc",
        atlas_engine::SymbolKind::Function,
    );

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    let args = serde_json::json!({
        "symbol": {
            "qualified_name": "MyFunc.MyFunc",
            "file_path": "src/main.ts"
        }
    });

    let (resp_str, is_error) = router.handle_symbol_detail(&args);
    assert!(
        !is_error,
        "Expected successful resolution, got error: {resp_str}"
    );
    assert!(
        !resp_str.contains("does not match any file"),
        "Should NOT contain file_path diagnostic on successful resolution, got: {resp_str}"
    );
}

// ── Focus runtime tests ───────────────────────────────────────────

#[test]
fn active_project_construction_wires_focus_runtime() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    // FocusRuntime is present at construction with materialize already injected.
    let mode = router.project().query_runtime.detect_access_strategy();
    assert_eq!(mode, atlas_engine::focus::runtime::AccessStrategy::Focus);
    assert!(
        router.project().materialize.has_structural_rebuilder(),
        "ActiveProject construction must wire Focus materialize"
    );
}

#[test]
fn focus_runtime_initialized_on_activate_project() {
    let store = test_store();
    let store2 = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let mode_before = router.project().query_runtime.detect_access_strategy();
    assert_eq!(
        mode_before,
        atlas_engine::focus::runtime::AccessStrategy::Focus
    );

    // Project switch rebuilds ActiveProject (and FocusRuntime) from construction.
    router.activate_project(PathBuf::from("/other"), store2);
    let mode_after = router.project().query_runtime.detect_access_strategy();
    assert_eq!(
        mode_after,
        atlas_engine::focus::runtime::AccessStrategy::Focus
    );
    assert!(router.project().materialize.has_structural_rebuilder());
}

#[test]
fn active_project_shares_focus_materialize_stack() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let project = router.project();
    assert!(
        project.materialize.has_structural_rebuilder(),
        "ActiveProject materialize must wire structural rebuilder"
    );
    assert!(
        project
            .analysis_runtime
            .dataflow()
            .has_structural_rebuilder(),
        "AnalysisRuntime must share configured Focus materialize dataflow"
    );
    assert!(
        project
            .query_runtime
            .focus_materialize_has_structural_rebuilder(),
        "FocusRuntime must use rebuilder-configured materialize at construction"
    );
    assert!(
        project
            .materialize
            .same_stack_as(project.engine.lock().unwrap().materialize()),
        "Engine must share the same FocusMaterialize Arc stack"
    );
    assert!(
        project
            .query_runtime
            .focus_materialize_same_stack_as(&project.materialize),
        "FocusRuntime must share the same FocusMaterialize Arc stack"
    );
}

// ── apply_focus_result_to_lr analysis guidance tests ─────────────

/// Mock SnapshotStore that captures stored snapshots in a Vec.
struct MockSnapshotStore {
    snapshots: Mutex<Vec<QuerySnapshot>>,
}

impl SnapshotStore for MockSnapshotStore {
    fn store_query_snapshot(&self, snapshot: QuerySnapshot) {
        self.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(snapshot);
    }
}

#[test]
fn test_focus_result_refinement_guidance_lives_in_analysis_not_work() {
    use atlas_engine::focus::job_tracker::JobTracker;
    use atlas_engine::structs::{AnswerQuality, CoverageTier, SemanticConfidence};
    use std::sync::Arc;

    // ── Helper: build JSON from FocusResult ──
    fn build_json(
        result: &atlas_engine::focus::runtime::FocusResult,
    ) -> (serde_json::Value, QuerySnapshot) {
        let lr = AnalysisEnvelope::new("test_tool", &serde_json::json!({}));
        let lr = apply_focus_result_to_lr(lr, result);
        let mock = MockSnapshotStore {
            snapshots: Mutex::new(Vec::new()),
        };
        let (json_str, _is_error) = lr.build(serde_json::json!({"result": "ok"}), &mock);
        let snapshot = mock.snapshots.lock().unwrap().pop().unwrap();
        (serde_json::from_str(&json_str).unwrap(), snapshot)
    }

    // ── Case 1: Terminal — job_tracker is None → assume all done ──
    {
        let result = atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: Some(AnswerQuality {
                coverage: CoverageTier::Partial { gaps: vec![] },
                confidence: SemanticConfidence::Medium,
            }),
            gaps: vec![atlas_engine::structs::KnownGap::BudgetExhausted {
                strategy: "test_pending".to_string(),
                remaining: 1,
            }],
            pending_closure_ids: vec!["cl_test_1".to_string(), "cl_test_2".to_string()],
            pending_extraction_job_ids: vec![],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: None,
        };

        let (resp, snapshot) = build_json(&result);
        assert!(snapshot.focus_result.is_some());

        assert!(
            resp.get("work").is_none(),
            "work must not be public: {resp}"
        );
        assert_eq!(
            resp["analysis"]["basis"],
            serde_json::json!(["manifest", "structural"])
        );
        // Terminal case: no retry_after_ms, no partial_result, no missing
        assert!(
            resp["analysis"].get("retry_after_ms").is_none(),
            "terminal should not have retry_after_ms: {resp}"
        );
        assert!(
            resp.get("partial_result").is_none(),
            "terminal should not have partial_result: {resp}"
        );
        assert!(
            resp["gaps"].as_array().is_some_and(|gaps| gaps.len() == 1),
            "terminal response should retain permanent gaps: {resp}"
        );
        assert_eq!(resp["gaps"][0]["scope"], "focus_closure");
        assert_eq!(resp["gaps"][0]["reason"], "budget_exhausted");
        assert!(resp["gaps"][0]["detail"].is_string());
        assert!(resp["gaps"][0].get("BudgetExhausted").is_none());
    }

    // ── Case 2: Non-terminal — tracker says jobs are pending ──
    {
        let tracker = JobTracker::new();
        // cl_test_1 is NOT marked done, so are_all_done returns false
        tracker.mark_done("cl_test_2");

        let result = atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: Some(AnswerQuality {
                coverage: CoverageTier::Partial { gaps: vec![] },
                confidence: SemanticConfidence::Medium,
            }),
            gaps: vec![atlas_engine::structs::KnownGap::BudgetExhausted {
                strategy: "test_pending".to_string(),
                remaining: 1,
            }],
            pending_closure_ids: vec!["cl_test_1".to_string(), "cl_test_2".to_string()],
            pending_extraction_job_ids: vec![],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: Some(Arc::new(tracker)),
        };

        let (resp, snapshot) = build_json(&result);
        let stored = snapshot.focus_result.expect("focus result stored");
        assert_eq!(stored.pending_closure_ids, result.pending_closure_ids);
        assert!(Arc::ptr_eq(
            stored.job_tracker.as_ref().unwrap(),
            result.job_tracker.as_ref().unwrap()
        ));

        assert!(
            resp.get("work").is_none(),
            "work must not be public: {resp}"
        );
        assert_eq!(resp["analysis"]["retry_after_ms"], 5000);
        assert_eq!(
            resp["analysis"]["summary"],
            "Focus analysis still expanding: 1 pending job(s) remaining."
        );
        assert!(
            resp.get("gaps").is_none(),
            "non-terminal response must suppress transient gaps: {resp}"
        );
        assert_eq!(
            resp["analysis"]["basis"],
            serde_json::json!(["manifest", "structural"])
        );
        // Non-terminal: no partial_result in the new protocol
        assert!(
            resp.get("partial_result").is_none(),
            "non-terminal should not set partial_result in new protocol: {resp}"
        );
    }

    // ── Case 2b: Non-terminal — foreground extraction job is in-flight ──
    {
        let result = atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: Some(AnswerQuality {
                coverage: CoverageTier::Partial { gaps: vec![] },
                confidence: SemanticConfidence::Medium,
            }),
            gaps: vec![atlas_engine::structs::KnownGap::BudgetExhausted {
                strategy: "should_be_suppressed_while_pending".to_string(),
                remaining: 1,
            }],
            pending_closure_ids: vec![],
            pending_extraction_job_ids: vec!["extract_pending".into()],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: None,
        };

        let (resp, snapshot) = build_json(&result);
        assert_eq!(resp["analysis"]["retry_after_ms"], 5000);
        assert_eq!(
            resp["analysis"]["summary"],
            "Focus analysis still expanding: 1 pending job(s) remaining."
        );
        assert!(
            resp.get("gaps").is_none(),
            "raw extraction pending is transient and must not expose gaps: {resp}"
        );
        assert_eq!(
            snapshot.focus_result.unwrap().pending_extraction_job_ids,
            vec!["extract_pending".to_string()]
        );
        assert_eq!(
            snapshot.status,
            crate::tools::query_snapshot::QueryStatus::Retryable
        );
    }

    // ── Case 3: Failed background work is terminal and diagnostic ──
    {
        let tracker = JobTracker::new();
        tracker.mark_failed("cl_failed", "fixture extraction failed");

        let result = atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: Some(AnswerQuality {
                coverage: CoverageTier::Partial { gaps: vec![] },
                confidence: SemanticConfidence::Medium,
            }),
            gaps: vec![],
            pending_closure_ids: vec!["cl_failed".to_string()],
            pending_extraction_job_ids: vec![],
            closure_id: None,
            seed_symbol_id: None,
            seed_file_id: None,
            built_files: vec![],
            coverage_counts: None,
            job_tracker: Some(Arc::new(tracker)),
        };

        let (resp, _) = build_json(&result);
        assert!(resp["analysis"].get("retry_after_ms").is_none());
        assert_eq!(resp["gaps"][0]["scope"], "focus_closure");
        assert_eq!(resp["gaps"][0]["reason"], "background_refinement_failed");
        assert!(
            resp["gaps"][0]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("fixture extraction failed"))
        );
    }
}

#[test]
fn resume_router_reuses_focus_result_without_preparing_new_work() {
    use atlas_engine::focus::job_tracker::JobTracker;
    use std::sync::Arc;

    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let tracker = Arc::new(JobTracker::new());
    let expected = atlas_engine::focus::runtime::FocusResult {
        access: atlas_engine::focus::runtime::AccessStrategy::Focus,
        quality: None,
        gaps: vec![],
        pending_closure_ids: vec!["existing_job".into()],
        pending_extraction_job_ids: vec![],
        closure_id: Some("existing_closure".into()),
        seed_symbol_id: None,
        seed_file_id: None,
        built_files: vec![],
        coverage_counts: None,
        job_tracker: Some(Arc::clone(&tracker)),
    };
    let replay = ToolRouter::for_resume(router.project(), Some(expected.clone()));
    let intent = atlas_engine::QueryIntent::Calls {
        symbol_name: "target".into(),
        file_id: None,
        symbol_id: None,
        direction: Some("outgoing".into()),
        depth: None,
    };

    let (actual, warnings) = replay.prepare_focus_query(Some(intent));
    let actual = actual.expect("replay focus result");
    assert!(warnings.is_empty());
    assert_eq!(actual.pending_closure_ids, expected.pending_closure_ids);
    assert!(Arc::ptr_eq(actual.job_tracker.as_ref().unwrap(), &tracker));
}

#[test]
fn resume_router_reprepares_after_raw_extraction_job_finishes() {
    let store = test_store();
    let file_id = register_test_file(&store, "src/main.ts");
    store
        .upsert_file_extraction_state(
            &file_id,
            "structural",
            "hash1",
            "complete",
            atlas_engine::structs::FactCoverage::default(),
        )
        .unwrap();
    store
        .claim_file_extraction_job(&file_id, "structural", Some("old_query"), None, None)
        .unwrap();
    let job_id = store
        .find_active_file_extraction_job(&file_id, "structural")
        .unwrap()
        .expect("claimed job should be active")
        .job_id;
    store.complete_extraction_job(&job_id).unwrap();

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let stale = atlas_engine::focus::runtime::FocusResult {
        access: atlas_engine::focus::runtime::AccessStrategy::Focus,
        quality: None,
        gaps: vec![],
        pending_closure_ids: vec![],
        pending_extraction_job_ids: vec![job_id],
        closure_id: Some("stale_closure".into()),
        seed_symbol_id: None,
        seed_file_id: Some(file_id),
        built_files: vec![],
        coverage_counts: None,
        job_tracker: None,
    };
    let replay = ToolRouter::for_resume(router.project(), Some(stale));
    let intent = atlas_engine::QueryIntent::Calls {
        symbol_name: "main".into(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: Some("outgoing".into()),
        depth: None,
    };

    let (actual, warnings) = replay.prepare_focus_query(Some(intent));
    let actual = actual.expect("resume should reprepare after raw job completion");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert!(
        actual.pending_extraction_job_ids.is_empty(),
        "completed raw extraction job must not keep resume_query pending"
    );
    assert_ne!(actual.closure_id.as_deref(), Some("stale_closure"));
}

#[test]
fn resume_query_converges_without_creating_another_snapshot() {
    use crate::tools::query_snapshot::QueryStatus;
    use atlas_engine::focus::job_tracker::JobTracker;
    use std::sync::Arc;

    let store = test_store();
    let caller_file = register_test_file(&store, "caller.ts");
    let callee_file = register_test_file(&store, "callee.ts");
    insert_test_symbol(&store, caller_file, "caller");
    insert_test_symbol(&store, callee_file, "callee");
    let caller = SymbolId::generate(&caller_file, "typescript", "caller", "function", None);
    let callee = SymbolId::generate(&callee_file, "typescript", "callee", "function", None);
    insert_test_edge(&store, caller, callee);

    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();
    let tracker = Arc::new(JobTracker::new());
    tracker.mark_done("finished_job");
    router.store_query_snapshot(QuerySnapshot {
        query_id: "q_original".into(),
        tool_name: "calls".into(),
        tool_args: serde_json::json!({
            "symbol": "caller.caller",
            "direction": "outgoing"
        }),
        focus_result: Some(atlas_engine::focus::runtime::FocusResult {
            access: atlas_engine::focus::runtime::AccessStrategy::Focus,
            quality: None,
            gaps: vec![],
            pending_closure_ids: vec!["finished_job".into()],
            pending_extraction_job_ids: vec![],
            closure_id: Some("original_closure".into()),
            seed_symbol_id: Some(caller),
            seed_file_id: Some(caller_file),
            built_files: vec![caller_file, callee_file],
            coverage_counts: None,
            job_tracker: Some(tracker),
        }),
        created_at: std::time::Instant::now(),
        status: QueryStatus::Retryable,
    });

    let (response, is_error) =
        router.handle_resume_query(&serde_json::json!({"query_id": "q_original"}));
    assert!(!is_error, "{response}");
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["query_id"], "q_original");
    assert_eq!(response["total_callees"], 1);
    assert!(response["analysis"].get("retry_after_ms").is_none());

    let project = router.project();
    let snapshots = project.job_runtime.query_snapshots.lock().unwrap();
    assert_eq!(snapshots.len(), 1, "temporary replay snapshot leaked");
    assert_eq!(snapshots["q_original"].status, QueryStatus::Ready);
}

// ── Contract-based dispatch tests ────────────────────────────────────

/// Verify that each tool name routes to the correct contract via `contract_for`.
#[test]
fn contract_based_dispatch_routes_to_correct_handler() {
    use crate::tools::tool_contract::{AnalysisNeeds, OverlayKind, QueryNeeds};
    use serde_json::json;

    // Project lifecycle
    assert_eq!(
        contract_for(
            "project",
            &json!({"action": "open", "project_path": "/tmp"})
        ),
        ToolContract::ProjectLifecycle
    );
    // Status read
    assert_eq!(
        contract_for("project", &json!({"action": "status"})),
        ToolContract::StatusRead
    );
    // Semantic graph queries
    assert_eq!(
        contract_for("calls", &json!({"symbol": "foo"})),
        ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
    );
    assert_eq!(
        contract_for("explore", &json!({"symbol": "foo"})),
        ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
    );
    assert_eq!(
        contract_for("path", &json!({"from": "a", "to": "b"})),
        ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
    );
    assert_eq!(
        contract_for("impact", &json!({"symbol": "foo"})),
        ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
    );
    assert_eq!(
        contract_for(
            "trace",
            &json!({"kind": "point", "file_path": "x.rs", "line": 1, "column": 1})
        ),
        ToolContract::TraceQuery(QueryNeeds::Full)
    );
    // Store fact queries
    assert_eq!(
        contract_for("symbol", &json!({"symbol": "foo"})),
        ToolContract::StoreFactQuery(QueryNeeds::Manifest)
    );
    assert_eq!(
        contract_for("search", &json!({"query": "foo"})),
        ToolContract::StoreFactQuery(QueryNeeds::Manifest)
    );
    assert_eq!(
        contract_for("file_dependencies", &json!({"file_path": "src/main.rs"})),
        ToolContract::StoreFactQuery(QueryNeeds::Structural)
    );
    // Semantic analysis
    assert_eq!(
        contract_for("branch_diff", &json!({"symbol": "foo"})),
        ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowEffects)
    );
    assert_eq!(
        contract_for("lifecycle", &json!({"symbol": "foo", "field": "ptr"})),
        ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowDomainRules)
    );
    // Overlay mutations
    assert_eq!(
        contract_for(
            "fp_dispatches",
            &json!({"action": "add", "field_qname": "f", "target_qname": "t"})
        ),
        ToolContract::OverlayMutation(OverlayKind::FunctionPointerDispatch)
    );
    assert_eq!(
        contract_for("fp_dispatches", &json!({"action": "list"})),
        ToolContract::OverlayRead
    );
    assert_eq!(
        contract_for(
            "domain_rules",
            &json!({"action": "add", "rule_kind": "free_fn", "pattern": "xfree"})
        ),
        ToolContract::OverlayMutation(OverlayKind::DomainRules)
    );
    assert_eq!(
        contract_for("domain_rules", &json!({"action": "list"})),
        ToolContract::OverlayRead
    );
    // Task control
    assert_eq!(contract_for("tasks", &json!({})), ToolContract::TaskControl);
    assert_eq!(
        contract_for("resume_query", &json!({"query_id": "abc"})),
        ToolContract::TaskControl
    );
}

/// Calling an unknown tool name must return is_error=true.
#[test]
fn unknown_tool_returns_error() {
    use serde_json::json;

    let store = test_store();
    let tmp = std::env::temp_dir();
    let router = ToolRouter::new_empty(store, tmp);
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, "nonexistent_tool", &json!({}));
    assert!(
        result.is_error.unwrap_or(false),
        "unknown tool should set is_error=true"
    );
}

/// Every tool registered via `make_all_tools()` must have a valid dispatch path
/// through `contract_for()` and the corresponding sub-dispatcher.
#[test]
fn contract_dispatch_handles_all_registered_tools() {
    use serde_json::json;

    let all_tools = make_all_tools();
    for tool in &all_tools {
        let name = &tool.name;
        let contract = contract_for(name, &json!({}));

        // Verify the contract routes to a sub-dispatcher that handles this name.
        let handled = tool_has_dispatch_path(name, &contract);
        assert!(
            handled,
            "tool '{name}' maps to contract {contract:?} but sub-dispatcher does not handle it"
        );
    }
}

/// Returns true if the given tool name has a matching arm in the sub-dispatcher
/// for its contract type.
fn tool_has_dispatch_path(name: &str, contract: &ToolContract) -> bool {
    match contract {
        // ProjectLifecycle → handled by handle_project directly
        ToolContract::ProjectLifecycle => true,
        // StatusRead → dispatch_status_read only handles "project"
        ToolContract::StatusRead => name == "project",
        // SemanticGraphQuery → dispatch_graph_query
        ToolContract::SemanticGraphQuery(_) => {
            matches!(name, "calls" | "explore" | "path" | "impact" | "symbol")
        }
        // TraceQuery → dispatch_trace_query
        ToolContract::TraceQuery(_) => {
            matches!(name, "trace")
        }
        // StoreFactQuery → dispatch_store_query
        ToolContract::StoreFactQuery(_) => {
            matches!(name, "symbol" | "search" | "file_dependencies")
        }
        // SemanticAnalysis → dispatch_analysis
        ToolContract::SemanticAnalysis(_) => {
            matches!(name, "branch_diff" | "lifecycle")
        }
        // OverlayMutation / OverlayRead → dispatch_overlay
        ToolContract::OverlayMutation(_) | ToolContract::OverlayRead => {
            matches!(name, "fp_dispatches" | "domain_rules")
        }
        // TaskControl → dispatch_task_control
        ToolContract::TaskControl => {
            matches!(name, "tasks" | "resume_query")
        }
    }
}

// ── E2E contract dispatch tests ────────────────────────────────────────
//
// These tests validate the full call_tool() → contract_for() → handler
// dispatch chain for every ToolContract variant.  They verify:
//   - The tool name routes to the correct contract
//   - The contract routes to the correct handler (no "Unknown tool" error)
//   - is_error field reflects expected behavior
//
// Handler logic is NOT tested here — each handler has its own unit tests.

/// Extract text from the first content block of a CallToolResult.
fn extract_text(result: &CallToolResult) -> String {
    match &result.content[0] {
        ContentBlock::Text { text } => text.clone(),
    }
}

// ── Test 1: ProjectLifecycle contract — "project" with action="open" ─

#[test]
fn e2e_project_lifecycle_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();
    // action=open is ProjectLifecycle; missing project_path will error
    // but the contract routing itself should work.
    let args = serde_json::json!({"action": "open"});
    let result = router.call_tool(&ctx, "project", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "project open should route to ProjectLifecycle, got: {text}"
    );
}

// ── Test 2: StatusRead contract — "project" with action="status"/"files" ─

#[test]
fn e2e_status_read_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();

    // action=status → StatusRead
    let args = serde_json::json!({"action": "status"});
    let result = router.call_tool(&ctx, "project", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "project status should route to StatusRead, got: {text}"
    );
    assert_eq!(result.is_error, Some(false), "status should succeed");

    // action=files → StatusRead
    let args2 = serde_json::json!({"action": "files"});
    let result2 = router.call_tool(&ctx, "project", &args2);
    let text2 = extract_text(&result2);
    assert!(
        !text2.contains("Unknown tool"),
        "project files should route to StatusRead, got: {text2}"
    );
    assert_eq!(result2.is_error, Some(false), "files should succeed");
}

// ── Test 4: SemanticGraphQuery contract — "calls" / "explore" ────────

#[test]
fn e2e_graph_query_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    // Graph must be initialized before SemanticGraphQuery tools can query.
    let _ = router.ensure_graph_initialized();
    let ctx = ToolCallContext::empty();

    // calls → SemanticGraphQuery(CallGraph)
    let args = serde_json::json!({"symbol": "test_func"});
    let result = router.call_tool(&ctx, "calls", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "calls should route to SemanticGraphQuery, got: {text}"
    );

    // explore → SemanticGraphQuery(CallGraph)
    let args2 = serde_json::json!({"symbol": "test_func"});
    let result2 = router.call_tool(&ctx, "explore", &args2);
    let text2 = extract_text(&result2);
    assert!(
        !text2.contains("Unknown tool"),
        "explore should route to SemanticGraphQuery, got: {text2}"
    );
}

// ── Test 5: StoreFactQuery contract — "search" / "symbol" ────────────

#[test]
fn e2e_store_query_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let _ = router.ensure_graph_initialized(); // "symbol" requires graph
    let ctx = ToolCallContext::empty();

    // search → StoreFactQuery(Manifest)
    let args = serde_json::json!({"query": "test"});
    let result = router.call_tool(&ctx, "search", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "search should route to StoreFactQuery, got: {text}"
    );

    // symbol with default view → StoreFactQuery(Manifest)
    let args2 = serde_json::json!({"symbol": "test"});
    let result2 = router.call_tool(&ctx, "symbol", &args2);
    let text2 = extract_text(&result2);
    assert!(
        !text2.contains("Unknown tool"),
        "symbol should route to StoreFactQuery, got: {text2}"
    );
}

// ── Test 6: SemanticAnalysis routing — "lifecycle" / "branch_diff" ──

#[test]
fn e2e_semantic_analysis_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();

    // lifecycle → SemanticAnalysis(CfgDataflowDomainRules)
    let args = serde_json::json!({"symbol": "test_func", "field": "data"});
    let result = router.call_tool(&ctx, "lifecycle", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "lifecycle should route to SemanticAnalysis, got: {text}"
    );

    // branch_diff → SemanticAnalysis(CfgDataflowEffects)
    let args2 = serde_json::json!({"symbol": "test_func"});
    let result2 = router.call_tool(&ctx, "branch_diff", &args2);
    let text2 = extract_text(&result2);
    assert!(
        !text2.contains("Unknown tool"),
        "branch_diff should route to SemanticAnalysis, got: {text2}"
    );
}

// ── Test 7: OverlayMutation / OverlayRead contract ────────────────────

#[test]
fn e2e_overlay_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();

    // domain_rules list → OverlayRead
    let args = serde_json::json!({"action": "list"});
    let result = router.call_tool(&ctx, "domain_rules", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "domain_rules list should route to OverlayRead, got: {text}"
    );

    // fp_dispatches list → OverlayRead
    let args2 = serde_json::json!({"action": "list"});
    let result2 = router.call_tool(&ctx, "fp_dispatches", &args2);
    let text2 = extract_text(&result2);
    assert!(
        !text2.contains("Unknown tool"),
        "fp_dispatches list should route to OverlayRead, got: {text2}"
    );

    // domain_rules add → OverlayMutation(DomainRules) — needs args, will
    // fail validation but routing should be correct.
    let args3 = serde_json::json!({"action": "add", "rule_kind": "free_fn", "pattern": "xfree"});
    let result3 = router.call_tool(&ctx, "domain_rules", &args3);
    let text3 = extract_text(&result3);
    assert!(
        !text3.contains("Unknown tool"),
        "domain_rules add should route to OverlayMutation, got: {text3}"
    );
}

// ── Test 8: TaskControl contract — "tasks" / "resume_query" ──────────

#[test]
fn e2e_task_control_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();

    // tasks → TaskControl
    let args = serde_json::json!({});
    let result = router.call_tool(&ctx, "tasks", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "tasks should route to TaskControl, got: {text}"
    );

    // resume_query → TaskControl
    let args2 = serde_json::json!({"query_id": "nonexistent"});
    let result2 = router.call_tool(&ctx, "resume_query", &args2);
    let text2 = extract_text(&result2);
    assert!(
        !text2.contains("Unknown tool"),
        "resume_query should route to TaskControl, got: {text2}"
    );
}

// ── Test 9: TraceQuery contract — "trace" tool (SemanticGraphQuery) ──

#[test]
fn e2e_trace_query_contract_routes_correctly() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let _ = router.ensure_graph_initialized();
    let ctx = ToolCallContext::empty();

    // trace kind=callers → SemanticGraphQuery(Full) → dispatch_graph_query
    let args = serde_json::json!({"kind": "callers", "symbol": "test_func"});
    let result = router.call_tool(&ctx, "trace", &args);
    let text = extract_text(&result);
    assert!(
        !text.contains("Unknown tool"),
        "trace should route via SemanticGraphQuery to graph handler, got: {text}"
    );
}

// ── Test 10: ctx forwarding — tools accept ToolCallContext::empty() ──

#[test]
fn e2e_ctx_forwarding_does_not_panic() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let _ = router.ensure_graph_initialized();
    let ctx = ToolCallContext::empty();

    // Tools that receive ctx: search, symbol, trace
    let cases: &[(&str, serde_json::Value)] = &[
        ("search", serde_json::json!({"query": "test", "scope": "."})),
        ("symbol", serde_json::json!({"symbol": "test"})),
        (
            "trace",
            serde_json::json!({"kind": "callers", "symbol": "test"}),
        ),
    ];

    for (tool_name, args) in cases {
        let result = router.call_tool(&ctx, tool_name, args);
        let text = extract_text(&result);
        assert!(
            !text.contains("Unknown tool"),
            "tool '{tool_name}' should accept empty ctx without panic, got: {text}"
        );
    }
}

// ── Bonus: non-existent tool returns StatusRead fallback ──────────────

#[test]
fn e2e_unknown_tool_falls_back_to_status_read() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({});
    let result = router.call_tool(&ctx, "nonexistent_tool", &args);
    let text = extract_text(&result);
    assert!(
        text.contains("Unknown tool"),
        "Unknown tool should return error via StatusRead fallback, got: {text}"
    );
    assert_eq!(
        result.is_error,
        Some(true),
        "unknown tool should be an error"
    );
}

#[test]
fn call_tool_truncates_large_results_with_visible_marker() {
    let store = test_store();
    for i in 0..600 {
        let path = format!(
            "src/very_long_directory_name_for_truncation_{i:04}/very_long_file_name_for_mcp_truncation_{i:04}.ts"
        );
        register_test_file(&store, &path);
    }
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    let ctx = ToolCallContext::empty();

    let result = router.call_tool(&ctx, "project", &serde_json::json!({"action": "files"}));

    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.content.len(),
        2,
        "large MCP responses must include a visible truncation marker"
    );
    let first = match &result.content[0] {
        ContentBlock::Text { text } => text,
    };
    assert!(
        first.len() <= 25000,
        "first content block must be bounded to 25KB, got {}",
        first.len()
    );
    let marker = match &result.content[1] {
        ContentBlock::Text { text } => text,
    };
    assert!(
        marker.contains("truncated") && marker.contains("showing first 25000"),
        "truncation marker must be explicit, got: {marker}"
    );
}

#[test]
fn semantic_graph_query_does_not_expose_internal_precision() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    insert_test_symbol(&store, file_id, "test_func");
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let _ = router.ensure_graph_initialized();
    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({"symbol": "test_func.test_func"});
    let result = router.call_tool(&ctx, "calls", &args);
    let text = extract_text(&result);
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        val.get("precision").is_none(),
        "graph tools must not expose internal precision: {text}"
    );
}

#[test]
fn does_not_inject_precision_for_non_graph_tools() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
    let _ = router.ensure_graph_initialized();
    let ctx = ToolCallContext::empty();
    let args = serde_json::json!({"action": "list"});
    let result = router.call_tool(&ctx, "domain_rules", &args);
    let text = extract_text(&result);
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        val.get("precision").is_none(),
        "should NOT inject precision for non-graph tools"
    );
}

// ── Phase 20: Error-path & boundary tests ──────────────────────────────

/// Contract dispatch returns error for graph tools without symbol.
#[test]
fn graph_tool_missing_symbol_returns_error() {
    let store = test_store();
    let tmp = tempfile::tempdir().unwrap();
    let router = ToolRouter::new_empty(store, tmp.path().to_path_buf());
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, "calls", &serde_json::json!({}));
    assert!(
        result.is_error == Some(true)
            || result.content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains("error") || text.contains("missing") || text.contains("symbol")
            }),
        "calls without symbol should return error"
    );
}

/// Contract dispatch returns error for lifecycle without field.
#[test]
fn lifecycle_missing_field_returns_error() {
    let store = test_store();
    let tmp = tempfile::tempdir().unwrap();
    let router = ToolRouter::new_empty(store, tmp.path().to_path_buf());
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, "lifecycle", &serde_json::json!({"symbol": "malloc"}));
    let text = extract_text(&result);
    assert!(
        result.is_error == Some(true)
            || text.contains("field")
            || text.contains("error")
            || text.contains("not found"),
        "lifecycle without field should return error, got: {text}"
    );
}

#[test]
fn lifecycle_unsupported_language_returns_terminal_gap() {
    let store = test_store();
    let file_id = register_test_file(&store, "a.ts");
    let symbol_id = insert_trace_test_symbol(
        &store,
        file_id,
        "handler",
        "handler",
        atlas_engine::SymbolKind::Function,
    );
    let entry = atlas_engine::CfgNode::entry(&symbol_id);
    let exit = atlas_engine::CfgNode::exit(&symbol_id);
    let edge = atlas_engine::CfgEdge::new(&entry.id, &exit.id, atlas_engine::CfgEdgeKind::Normal);
    store.insert_cfg_nodes(&[entry, exit]).unwrap();
    store.insert_cfg_edges(&[edge]).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let router = ToolRouter::new_empty(store, tmp.path().to_path_buf());
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(
        &ctx,
        "lifecycle",
        &serde_json::json!({"symbol": "handler", "field": "ptr"}),
    );
    let text = extract_text(&result);
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(result.is_error, Some(false), "{text}");
    assert_eq!(val["ok"], serde_json::json!(false), "{text}");
    assert_eq!(
        val["error"],
        serde_json::json!("unsupported_language"),
        "{text}"
    );
    assert!(
        val.get("verdict").is_none(),
        "unsupported language is a capability gap, not an analysis verdict: {text}"
    );
    assert_eq!(
        val["gaps"][0]["reason"],
        serde_json::json!("unsupported_language"),
        "{text}"
    );
    assert!(
        val["analysis"].get("retry_after_ms").is_none(),
        "unsupported language is terminal for lifecycle and should not be retryable: {text}"
    );
}

/// Contract dispatch returns error for branch_diff without symbol.
#[test]
fn branch_diff_missing_symbol_returns_error() {
    let store = test_store();
    let tmp = tempfile::tempdir().unwrap();
    let router = ToolRouter::new_empty(store, tmp.path().to_path_buf());
    let ctx = ToolCallContext::empty();
    let result = router.call_tool(&ctx, "branch_diff", &serde_json::json!({}));
    assert!(
        result.is_error == Some(true)
            || result.content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains("error") || text.contains("symbol") || text.contains("missing")
            }),
        "branch_diff without symbol should return error"
    );
}

/// Symbol tool handles all view modes through correct contracts.
#[test]
fn symbol_tool_routes_all_views_correctly() {
    let store = test_store();
    let tmp = tempfile::tempdir().unwrap();
    let router = ToolRouter::new_empty(store, tmp.path().to_path_buf());
    let _ = router.ensure_graph_initialized();
    let ctx = ToolCallContext::empty();

    // view=detail → StoreFactQuery (no graph needed)
    // In focus mode, symbol-not-found can return either a hard error or
    // a retryable unresolved result (is_error=false with analysis.retry_after_ms).
    let r1 = router.call_tool(
        &ctx,
        "symbol",
        &serde_json::json!({"symbol": "main", "view": "detail"}),
    );
    let t1 = extract_text(&r1);
    assert!(
        r1.is_error == Some(true)
            || t1.contains("not found")
            || t1.contains("error")
            || t1.contains("unresolved")
            || t1.contains("not available"),
        "symbol view=detail should not panic, got: {t1}"
    );

    // view=context → SemanticGraphQuery (needs graph)
    let r2 = router.call_tool(
        &ctx,
        "symbol",
        &serde_json::json!({"symbol": "main", "view": "context"}),
    );
    let t2 = extract_text(&r2);
    assert!(
        r2.is_error == Some(true)
            || t2.contains("not found")
            || t2.contains("error")
            || t2.contains("unresolved")
            || t2.contains("not available"),
        "symbol view=context should not panic, got: {t2}"
    );

    // view=usages → StoreFactQuery
    let r3 = router.call_tool(
        &ctx,
        "symbol",
        &serde_json::json!({"symbol": "main", "view": "usages"}),
    );
    let t3 = extract_text(&r3);
    assert!(
        r3.is_error == Some(true)
            || t3.contains("not found")
            || t3.contains("error")
            || t3.contains("unresolved")
            || t3.contains("not available"),
        "symbol view=usages should not panic, got: {t3}"
    );
}

// ── Phase 21a: maybe_refresh_graph cooldown ────────────────────────

#[test]
fn maybe_refresh_skips_when_generation_unchanged() {
    let store = test_store();
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    // Record initial generation — graph is fresh after construction.
    let gen_before = router
        .project()
        .graph_runtime
        .last_graph_generation
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(!router.project().graph_runtime.is_graph_stale());

    // Call maybe_refresh_graph — no lazy writes, no generation bump → no rebuild.
    router.maybe_refresh_graph().unwrap();
    assert!(!router.project().graph_runtime.is_graph_stale());
    assert_eq!(
        router
            .project()
            .graph_runtime
            .last_graph_generation
            .load(std::sync::atomic::Ordering::Relaxed),
        gen_before
    );
}

#[test]
fn maybe_refresh_bumps_generation_after_lazy_writes() {
    let store = test_store();
    let file_id = register_test_file(&store, "test.ts");
    let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
    router.ensure_graph_initialized().unwrap();

    // Pre-populate lazy_refresh_queue with a dummy file_id.
    router
        .project()
        .query_runtime
        .lazy_refresh_queue
        .record_lazy_writes(&[file_id]);

    let gen_before = router
        .project()
        .graph_runtime
        .last_graph_generation
        .load(std::sync::atomic::Ordering::Relaxed);

    // Call maybe_refresh_graph → batch is non-empty → must bump graph_generation
    // and trigger rebuild.
    router.maybe_refresh_graph().unwrap();

    // After lazy writes flushed, graph should be marked fresh (rebuilt).
    assert!(!router.project().graph_runtime.is_graph_stale());
    assert!(
        router
            .project()
            .graph_runtime
            .last_graph_generation
            .load(std::sync::atomic::Ordering::Relaxed)
            > gen_before,
        "lazy writes should bump graph_generation"
    );
}

// ── Phase 22a: Graph refresh boundary tests ────────────────────────

/// Verify graph state is unchanged after refresh with empty file batch.
#[test]
fn graph_refresh_with_empty_batch_is_noop() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp/test"));
    router.ensure_graph_initialized().unwrap();

    let node_before = router.project().graph_runtime.state.symbol_count();
    let edge_before = router.project().graph_runtime.state.edge_count();

    router
        .project()
        .graph_runtime
        .state
        .refresh_graph_for_files(&store, &[])
        .unwrap();

    assert_eq!(
        router.project().graph_runtime.state.symbol_count(),
        node_before
    );
    assert_eq!(
        router.project().graph_runtime.state.edge_count(),
        edge_before
    );
}

/// Verify graph counts increase after external store writes add symbols.
#[test]
fn graph_refresh_after_external_store_write_updates_graph() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp/test"));
    router.ensure_graph_initialized().unwrap();

    let node_before = router.project().graph_runtime.state.symbol_count();
    assert_eq!(
        node_before, 0,
        "empty graph should have 0 nodes, got {node_before}"
    );

    let file_id = register_test_file(&store, "src/test.ts");
    insert_test_symbol(&store, file_id, "foo");

    router
        .project()
        .graph_runtime
        .invalidation
        .graph_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    assert!(router.project().graph_runtime.is_graph_stale());

    router.maybe_refresh_graph().unwrap();

    let node_after = router.project().graph_runtime.state.symbol_count();
    assert!(
        node_after > 0,
        "node count should increase after external store write + refresh, got {node_after}"
    );
}

/// Verify externally written graph facts survive an incremental refresh.
#[test]
fn graph_refresh_preserves_existing_edges() {
    let store = test_store();
    let router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp/test"));
    router.ensure_graph_initialized().unwrap();

    let file_id = register_test_file(&store, "src/test.ts");
    insert_test_symbol(&store, file_id, "foo");
    router
        .project()
        .graph_runtime
        .invalidation
        .graph_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    router.maybe_refresh_graph().unwrap();

    let node_before = router.project().graph_runtime.state.symbol_count();
    let edge_before = router.project().graph_runtime.state.edge_count();
    assert!(
        node_before > 0,
        "should have nodes after initial store write + refresh, got {node_before}"
    );

    router.maybe_refresh_graph().unwrap();

    assert_eq!(
        router.project().graph_runtime.state.symbol_count(),
        node_before,
        "second refresh within cooldown should not change node count"
    );
    assert_eq!(
        router.project().graph_runtime.state.edge_count(),
        edge_before,
        "second refresh within cooldown should not change edge count"
    );
}

// ── BUG-6: fresh-request background closure refresh ──────────────

/// A non-replay request has no FocusResult carrying closure IDs from prior
/// requests. The runtime-owned refresh feed must still publish a completed
/// background job's graph writes before the next graph query.
#[test]
fn maybe_refresh_without_replay_drains_runtime_background_built_files_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = test_store();
    let caller_file = register_test_file(&store, "src/caller.ts");
    let callee_file = register_test_file(&store, "src/callee.ts");
    for file_id in [caller_file, callee_file] {
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "hash1",
                "complete",
                atlas_engine::FactCoverage::default(),
            )
            .unwrap();
    }
    insert_test_symbol(&store, caller_file, "caller");
    insert_test_symbol(&store, callee_file, "callee");
    let caller_id = SymbolId::generate(&caller_file, "typescript", "caller", "function", None);
    let callee_id = SymbolId::generate(&callee_file, "typescript", "callee", "function", None);

    let router = ToolRouter::new_empty(store.clone(), tmp.path().to_path_buf());
    router.ensure_graph_initialized().unwrap();
    assert!(
        router
            .project()
            .graph_runtime
            .provider()
            .graph_snapshot()
            .unwrap()
            .callees(&caller_id)
            .callees
            .is_empty()
    );

    // Obtain the runtime-owned tracker through a normal foreground result.
    // SemanticFunction is seed-only and does not enqueue graph expansion.
    let (focus_result, warnings) =
        router.prepare_focus_query(Some(atlas_engine::QueryIntent::SemanticFunction {
            symbol_name: "caller".into(),
            file_id: Some(caller_file),
            symbol_id: Some(caller_id),
        }));
    assert!(
        warnings.is_empty(),
        "unexpected focus warnings: {warnings:?}"
    );
    let tracker = focus_result.unwrap().job_tracker.unwrap();

    // Simulate the scheduler's post-build publication after graph init.
    insert_test_edge(&store, caller_id, callee_id);
    tracker.record_built_files("bg_cl_1", [caller_file, caller_file]);
    tracker.mark_done("bg_cl_1");

    let gen_before = router
        .project()
        .graph_runtime
        .last_graph_generation
        .load(std::sync::atomic::Ordering::Relaxed);

    // This is a fresh request path: replay_focus_result is None.
    router.maybe_refresh_graph().unwrap();

    assert!(
        router
            .project()
            .graph_runtime
            .last_graph_generation
            .load(std::sync::atomic::Ordering::Relaxed)
            > gen_before,
        "fresh refresh should consume the runtime background feed"
    );
    let graph = router
        .project()
        .graph_runtime
        .provider()
        .graph_snapshot()
        .unwrap();
    assert_eq!(graph.callees(&caller_id).callees.len(), 1);

    let gen_after_first = router
        .project()
        .graph_runtime
        .last_graph_generation
        .load(std::sync::atomic::Ordering::Relaxed);
    router.maybe_refresh_graph().unwrap();
    assert_eq!(
        router
            .project()
            .graph_runtime
            .last_graph_generation
            .load(std::sync::atomic::Ordering::Relaxed),
        gen_after_first,
        "the one-shot runtime feed must not republish the same file"
    );
}
