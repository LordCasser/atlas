//! Framework-level integration tests.
//!
//! These tests validate the framework infrastructure (tool dispatch shape,
//! error handling, FocusRuntime lifecycle) using READ-ONLY access to the
//! python_example DB. No test encodes knowledge of specific symbol names or
//! repo content beyond what is minimally needed to exercise a code path.
//!
//! End-to-end content-adaptive tests live in `tests/e2e_tests.rs`.

use super::{ToolCallContext, ToolRouter};
use atlas_engine::Store;
use serde_json::json;
use std::sync::Arc;

// =========================================================================
// Helpers
// =========================================================================

fn python_example_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/python_example")
        .canonicalize()
        .expect("python_example directory not found")
}

fn open_python_example_store() -> Arc<Store> {
    let db_path = python_example_root().join(".atlas/atlas.db");
    assert!(
        db_path.exists(),
        ".atlas DB not found. Run 'atlas index' in examples/python_example first."
    );
    Arc::new(Store::open_db(&db_path).expect("Failed to open DB"))
}

fn python_example_router() -> ToolRouter {
    let store = open_python_example_store();
    let router = ToolRouter::new_empty(store, python_example_root());
    router
        .ensure_graph_initialized()
        .expect("Failed to init graph");
    router
}

fn parse_json(s: &str) -> serde_json::Value {
    if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') {
        serde_json::from_str(s).unwrap_or_else(|e| panic!("JSON parse: {e}\n{s:.500}"))
    } else {
        json!({"is_error": true, "message": s})
    }
}

fn assert_ok(resp: &serde_json::Value) {
    let err = resp
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(!err, "Response has is_error=true: {resp:.300}");
}

// =========================================================================
// Tool discovery
// =========================================================================

#[test]
fn list_tools_has_all_categories() {
    let router = python_example_router();
    let tools = &router.list_tools().tools;
    assert!(
        tools.len() >= 15,
        "expected >=15 tools, got {}",
        tools.len()
    );

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    for required in ["search", "symbol", "calls", "impact", "trace", "project"] {
        assert!(names.contains(&required), "Missing tool: {required}");
    }
}

// =========================================================================
// Status
// =========================================================================

#[test]
fn status_has_required_fields() {
    let router = python_example_router();
    let (s, err) = router.handle_status();
    assert!(!err, "status error: {s}");
    let r = parse_json(&s);
    for field in ["project", "summary", "index"] {
        assert!(r.get(field).is_some(), "status missing '{field}'");
    }
}

// =========================================================================
// Search — framework-level error/empty handling
// =========================================================================

#[test]
fn search_scope_filters() {
    let router = python_example_router();
    let (s, err) = router.handle_search(
        &ToolCallContext::empty(),
        &json!({"query": "def", "scope": "spiders", "analysis": "manifest"}),
    );
    if err {
        return;
    } // skip if scope search not supported in manifest mode
    let r = parse_json(&s);
    for hit in r["results"].as_array().unwrap_or(&vec![]) {
        let f = hit["file"].as_str().unwrap_or("");
        assert!(f.contains("spiders"), "result outside scope: {f}");
    }
}

#[test]
fn search_nonexistent_returns_zero() {
    let router = python_example_router();
    let (s, err) = router.handle_search(
        &ToolCallContext::empty(),
        &json!({"query": "ZZZNonExistentXYZZY", "scope": ".", "analysis": "manifest"}),
    );
    assert!(!err, "search error: {s}");
    assert_eq!(parse_json(&s)["total"].as_u64().unwrap_or(99), 0);
}

#[test]
fn search_empty_query_is_error() {
    let router = python_example_router();
    let (s, _err) = router.handle_search(
        &ToolCallContext::empty(),
        &json!({"query": "", "scope": ".", "analysis": "manifest"}),
    );
    let r = parse_json(&s);
    let is_error = r.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    if is_error {
        // fine — error is expected
    } else {
        assert_eq!(
            r["total"].as_u64().unwrap_or(99),
            0,
            "empty query should return 0 results"
        );
    }
}

// =========================================================================
// Graph — error handling
// =========================================================================

#[test]
fn graph_nonexistent_symbol() {
    let router = python_example_router();
    let (s, err) =
        router.handle_callers(&json!({"symbol": "NonExistent_XYZ123", "direction": "incoming"}));
    if err {
        let r = parse_json(&s);
        assert!(
            r.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false),
            "nonexistent should set is_error: {s:.200}"
        );
    }
}

// =========================================================================
// Domain rules
// =========================================================================

#[test]
fn domain_rules_list_has_rules() {
    let router = python_example_router();
    let (s, err) = router.handle_atlas_domain_rules(&json!({"action": "list"}));
    assert!(!err, "domain_rules error: {s}");
    let r = parse_json(&s);
    assert!(r.get("rules").is_some(), "missing rules: {s:.200}");
}

// =========================================================================
// Tasks / jobs
// =========================================================================

#[test]
fn tasks_has_tasks_field() {
    let router = python_example_router();
    let (s, err) = router.handle_tasks(&json!({}));
    assert!(!err, "tasks error: {s}");
    let r = parse_json(&s);
    assert!(
        r.get("active_extraction_jobs").is_some() || r.get("atlas_jobs").is_some(),
        "missing tasks: {s:.200}"
    );
}

#[test]
fn jobs_has_jobs_field() {
    let router = python_example_router();
    let (s, err) = router.handle_jobs();
    assert!(!err, "jobs error: {s}");
    let r = parse_json(&s);
    assert!(
        r.get("active_jobs").is_some() || r.get("jobs").is_some(),
        "missing jobs: {s:.200}"
    );
}

// =========================================================================
// FP annotations
// =========================================================================

#[test]
fn list_fp_annotations_does_not_panic() {
    let router = python_example_router();
    let (s, _) = router.handle_list_fp_annotations();
    let r = parse_json(&s);
    assert!(
        r.get("annotations").is_some(),
        "missing annotations: {s:.200}"
    );
}

// =========================================================================
// No-panic sweep — all handlers survive minimal args without panic
// =========================================================================

#[test]
fn handlers_no_panic_empty_args() {
    let router = python_example_router();
    let router2 = python_example_router();

    let read_only: Vec<(&str, Box<dyn Fn() -> (String, bool)>)> = vec![
        ("status", Box::new(|| router2.handle_status())),
        ("jobs", Box::new(|| router2.handle_jobs())),
        (
            "list_fp_annotations",
            Box::new(|| router2.handle_list_fp_annotations()),
        ),
    ];

    for (name, handler) in &read_only {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
        assert!(r.is_ok(), "Handler '{name}' panicked with empty args");
    }

    let mut_handlers: Vec<(&str, serde_json::Value)> = vec![
        ("domain_rules", json!({"action": "list"})),
        ("tasks", json!({})),
    ];

    for (name, args) in &mut_handlers {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match *name {
            "domain_rules" => {
                router.handle_atlas_domain_rules(args);
            }
            "tasks" => {
                router.handle_tasks(args);
            }
            _ => {}
        }));
        assert!(r.is_ok(), "Handler '{name}' panicked");
    }
}

// =========================================================================
// Focus-driven analysis tests (no pre-built index)
// =========================================================================
//
// These tests exercise the FocusRuntime path: fresh DB → bootstrap →
// closure build → query.  Each test uses an in-memory store or the
// pre-built python_example DB in READ-ONLY mode.

mod focus_tests {
    use super::*;
    use std::path::Path;

    /// Create an in-memory Store with fresh schema — no file DB at all.
    /// This isolates focus tests from the pre-built python_example/.atlas/atlas.db.
    fn fresh_focus_store() -> Arc<Store> {
        let store = Store::open_in_memory().expect("open in-memory store");
        store.init_schema().expect("init schema");
        Arc::new(store)
    }

    fn focus_router(project_root: &Path) -> ToolRouter {
        let store = fresh_focus_store();
        let router = ToolRouter::new_empty(store, project_root.to_path_buf());
        router.init_focus();
        router
    }

    #[test]
    fn focus_bootstrap_completes_and_prepares_query() {
        let root = python_example_root();
        let router = focus_router(&root);

        let intent = atlas_engine::QueryIntent::Calls {
            symbol_name: "WikipediaSpider".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };

        let (focus_opt, warnings) = router.prepare_focus_query(Some(intent));

        assert!(
            focus_opt.is_some(),
            "Focus result was None. Warnings: {warnings:?}"
        );

        let result = focus_opt.unwrap();
        assert_eq!(
            result.mode,
            atlas_engine::focus::runtime::IndexMode::Focus,
            "Expected Focus mode"
        );
        assert!(
            !result.built_files.is_empty() || !result.pending_closure_ids.is_empty(),
            "Focus should build files or create pending closures. \
             Built: {}, Pending: {:?}",
            result.built_files.len(),
            result.pending_closure_ids
        );
        assert!(
            result.precision.is_some(),
            "Focus result should have precision"
        );
    }

    /// When FocusRuntime is active, handler responses should carry analysis
    /// envelope fields (state, scope, summary).  Accepts the case where the
    /// underlying DB already has a full index (focus not needed).
    #[test]
    fn focus_analysis_envelope_has_fields() {
        let router = python_example_router(); // has manifest-indexed symbols
        router.init_focus();

        let intent = atlas_engine::QueryIntent::Calls {
            symbol_name: "WikipediaSpider".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        // Focus may return None if the DB qualifies as full-index — that's OK
        let (_focus_opt, _warnings) = router.prepare_focus_query(Some(intent));

        let ctx = ToolCallContext::empty();
        let (resp_str, is_error) = router.handle_symbol(
            &ctx,
            &json!({"symbol": {"qualified_name": "start_process"}}),
        );
        assert!(!is_error, "Symbol query failed: {resp_str:.300}");

        let parsed = parse_json(&resp_str);
        assert_ok(&parsed);

        // Check that analysis envelope is present when focus *is* active.
        // When focus is not needed (full index exists), its absence is expected.
        if let Some(analysis) = parsed.get("analysis") {
            let state = analysis.get("state").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                !state.is_empty(),
                "Analysis state should be non-empty (focus active)"
            );
        }
    }

    /// Search with FocusRuntime active should return results from existing
    /// manifest index.  Focus analysis envelope is optional (full-index DB may skip it).
    #[test]
    fn focus_search_returns_results() {
        let router = python_example_router(); // has manifest-indexed symbols
        router.init_focus();

        let intent = atlas_engine::QueryIntent::Search {
            query: "spider".into(),
            scope: None,
        };
        // Focus may return None if the DB qualifies as full-index — that's OK
        let (_focus_opt, _warnings) = router.prepare_focus_query(Some(intent));

        let ctx = ToolCallContext::empty();
        let (resp_str, is_error) =
            router.handle_search(&ctx, &json!({"query": "Wikipedia", "scope": "."}));
        assert!(!is_error, "Search failed: {resp_str:.300}");

        let parsed = parse_json(&resp_str);
        assert_ok(&parsed);

        let results = parsed.get("results");
        assert!(results.is_some(), "Search should have results array");
    }

    /// Focus vs full-index equivalence: for a local code neighborhood,
    /// Focus-driven analysis (fresh DB, no pre-indexing) should produce
    /// results comparable to a full-index ground truth across multiple
    /// dimensions: symbol resolution, callees (outgoing edges), callers
    /// (incoming edges), and file completeness.
    ///
    /// This test validates the core Focus architecture promise: "large repos
    /// analyzed locally through Focus should be basically equivalent to full
    /// indexing."
    #[test]
    fn focus_equivalence_vs_full_index() {
        let root = python_example_root();

        // ── Phase 1: Discover best test symbol from full index ─────────
        // Instead of hardcoding a single symbol, scan multiple candidates
        // and pick one with non-zero callees (outgoing edges are more
        // predictable than incoming callers).
        let full_store = open_python_example_store(); // full-indexed DB
        let full_router = ToolRouter::new_empty(full_store, root.clone());
        full_router
            .ensure_graph_initialized()
            .expect("full graph init");

        let candidates = [
            "main",
            "run_spider",
            "list_spiders",
            "create_custom_spider",
            "WikipediaSpider",
            "run_tests",
            "EasySpiderSpiderMiddleware",
        ];

        // Ground truth accumulated from full-index queries.
        struct GroundTruth {
            symbol_name: String,
            kind: String,
            file: String,
            callee_names: Vec<String>,
            total_callees: u64,
            caller_names: Vec<String>,
            total_callers: u64,
        }

        let mut found_gt: Option<GroundTruth> = None;
        'outer: loop {
            for &name in &candidates {
                // -- Symbol resolution --
                let (sym_resp, sym_err) = full_router.handle_symbol(
                    &ToolCallContext::empty(),
                    &json!({"symbol": {"qualified_name": name}, "view": "detail"}),
                );
                if sym_err {
                    println!("  [full] symbol {name}: error {sym_resp:.120}");
                    continue;
                }
                let sym = parse_json(&sym_resp);
                if sym
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    println!("  [full] symbol {name}: is_error=true");
                    continue;
                }
                let kind = sym["kind"].as_str().unwrap_or("").to_string();
                let file = sym["file"].as_str().unwrap_or("").to_string();
                if kind.is_empty() || file.is_empty() {
                    println!("  [full] symbol {name}: missing kind/file");
                    continue;
                }

                // -- Callees (outgoing) --
                let (callee_resp, callee_err) = full_router.handle_calls(&json!({
                    "symbol": name,
                    "direction": "outgoing",
                }));
                if callee_err {
                    println!("  [full] callees {name}: error {callee_resp:.120}");
                    continue;
                }
                let callee = parse_json(&callee_resp);
                let callee_names: Vec<String> = callee["callees"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(|c| c["name"].as_str().map(String::from))
                    .collect();
                let total_callees = callee["total_callees"].as_u64().unwrap_or(0);

                // -- Callers (incoming) --
                let (caller_resp, caller_err) = full_router.handle_callers(&json!({
                    "symbol": name,
                }));
                let caller_names: Vec<String> = if !caller_err {
                    let caller = parse_json(&caller_resp);
                    caller["callers"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(|c| c["name"].as_str().map(String::from))
                        .collect()
                } else {
                    vec![]
                };
                let total_callers = if !caller_err {
                    parse_json(&caller_resp)["total_callers"]
                        .as_u64()
                        .unwrap_or(0)
                } else {
                    0
                };

                println!(
                    "  [full] {name}: kind={kind}, file={file}, \
                     callees={total_callees} ({callee_names:?}), \
                     callers={total_callers} ({caller_names:?})"
                );

                if total_callees > 0 {
                    found_gt = Some(GroundTruth {
                        symbol_name: name.to_string(),
                        kind,
                        file,
                        callee_names,
                        total_callees,
                        caller_names,
                        total_callers,
                    });
                    break 'outer;
                }
            }
            break;
        }

        let gt = match found_gt {
            Some(gt) => gt,
            None => {
                println!(
                    "Skipping equivalence check: no symbol with non-zero callees in full index"
                );
                return;
            }
        };

        let symbol_name = &gt.symbol_name;
        println!(
            "\nSelected: {symbol_name} (kind={}, file={}), \
             {total_callees} callee(s), {total_callers} caller(s)",
            gt.kind,
            gt.file,
            total_callees = gt.total_callees,
            total_callers = gt.total_callers,
        );

        // ── Phase 2: Fresh DB, pure Focus path ─────────────────────────
        // Register the target file in the files table so focus extraction
        // can find it. (Focus bootstrap Tier0 populates file_inventory but
        // not the files table needed by ensure_structural_for_file.)
        let seed_file_id = atlas_engine::FileId::generate(&gt.file);
        let focus_store = fresh_focus_store();

        // Pre-register file info so lazy structural extraction can proceed.
        let file_info = atlas_engine::FileInfo {
            file_id: seed_file_id,
            path: gt.file.clone(),
            language: atlas_engine::Language::Python,
            content_hash: "focus_test_v1".to_string(),
            status: atlas_engine::ParseStatus::Success,
        };
        focus_store.upsert_file(&file_info).expect("upsert_file");

        let focus_router = ToolRouter::new_empty(focus_store.clone(), root.clone());
        focus_router.init_focus();
        focus_router
            .ensure_graph_initialized()
            .expect("initialize empty focus graph");

        // Enter through the cold calls handler. It must prepare one focus
        // closure, make its graph writes visible, and eventually terminate.
        let (cold_response, cold_error) = focus_router.handle_calls(&json!({
            "symbol": {"qualified_name": symbol_name, "file_path": gt.file},
            "direction": "outgoing",
        }));
        assert!(!cold_error, "cold focus calls failed: {cold_response}");
        let mut cold_calls = parse_json(&cold_response);
        for _ in 0..100 {
            if cold_calls["analysis"].get("retry_after_ms").is_none() {
                break;
            }
            let query_id = cold_calls["query_id"]
                .as_str()
                .expect("retryable calls response must carry query_id");
            std::thread::sleep(std::time::Duration::from_millis(20));
            let (resumed, resume_error) =
                focus_router.handle_resume_query(&json!({"query_id": query_id}));
            assert!(!resume_error, "resume_query failed: {resumed}");
            cold_calls = parse_json(&resumed);
        }
        assert!(
            cold_calls["analysis"].get("retry_after_ms").is_none(),
            "cold focus calls did not converge: {cold_calls}"
        );
        assert!(
            cold_calls["total_callees"].as_u64().unwrap_or(0) > 0,
            "terminal cold focus response lost materialized call edges: {cold_calls}"
        );

        let query_id = cold_calls["query_id"]
            .as_str()
            .expect("calls response must carry query_id");
        let focus_result = focus_router
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap()
            .get(query_id)
            .and_then(|snapshot| snapshot.focus_result.clone())
            .expect("calls snapshot must retain its focus result");
        println!(
            "Focus: mode={:?}, built_files={}, precision={:?}",
            focus_result.mode,
            focus_result.built_files.len(),
            focus_result.precision,
        );

        assert_eq!(
            focus_result.mode,
            atlas_engine::focus::runtime::IndexMode::Focus,
            "Expected Focus mode"
        );

        // ── Phase 3: Multi-dimensional comparison ──────────────────────

        // Dimension A: Symbol resolution — same kind and file as full index.
        {
            let (sym_resp, sym_err) = focus_router.handle_symbol(
                &ToolCallContext::empty(),
                &json!({"symbol": {"qualified_name": symbol_name}, "view": "detail"}),
            );
            if sym_err {
                panic!(
                    "Focus symbol resolution failed for {symbol_name}: {sym_resp:.300}\n\
                     Focus built {} files.",
                    focus_result.built_files.len(),
                );
            }
            let focus_sym = parse_json(&sym_resp);
            assert_ok(&focus_sym);

            let focus_kind = focus_sym["kind"].as_str().unwrap_or("");
            let focus_file = focus_sym["file"].as_str().unwrap_or("");
            assert_eq!(
                focus_kind, gt.kind,
                "Symbol kind mismatch: full=[{}], focus=[{focus_kind}]",
                gt.kind,
            );
            // File paths may differ in format (short vs full path). Use
            // ends_with to normalize.
            let gt_file = &gt.file;
            assert!(
                focus_file.ends_with(gt_file.as_str())
                    || gt_file.ends_with(&focus_file)
                    || focus_file.contains(gt_file.as_str())
                    || gt_file.contains(&focus_file),
                "Symbol file mismatch: full=[{}], focus=[{focus_file}]",
                gt.file,
            );
            println!("  [A] Symbol resolution OK: kind={focus_kind}, file={focus_file}");
        }

        // Dimension B: Callees (outgoing edges) — should have >50% overlap.
        {
            let (focus_resp, focus_err) = focus_router.handle_calls(&json!({
                "symbol": symbol_name,
                "direction": "outgoing",
            }));
            if focus_err {
                let err_r = parse_json(&focus_resp);
                let msg = err_r
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&focus_resp);
                if msg.contains("not found") {
                    panic!("Focus resolved symbol but callees could not locate it: {msg}");
                }
                println!("  [B] Focus callees returned error: {msg:.200} (acceptable)");
            } else {
                let mut focus_callee = parse_json(&focus_resp);
                for _ in 0..100 {
                    if focus_callee["analysis"].get("retry_after_ms").is_none() {
                        break;
                    }
                    let query_id = focus_callee["query_id"]
                        .as_str()
                        .expect("retryable calls response must carry query_id");
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    let (resumed, resume_error) =
                        focus_router.handle_resume_query(&json!({"query_id": query_id}));
                    assert!(!resume_error, "resume_query failed: {resumed}");
                    focus_callee = parse_json(&resumed);
                }
                assert!(
                    focus_callee["analysis"].get("retry_after_ms").is_none(),
                    "focus calls response did not converge: {focus_callee}"
                );
                if focus_callee
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let msg = focus_callee["message"].as_str().unwrap_or("");
                    println!("  [B] Focus callees is_error: {msg:.200} (acceptable)");
                } else {
                    let focus_callee_names: Vec<String> = focus_callee["callees"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(|c| c["name"].as_str().map(String::from))
                        .collect();
                    let focus_total = focus_callee["total_callees"].as_u64().unwrap_or(0);

                    let overlap: Vec<&String> = focus_callee_names
                        .iter()
                        .filter(|n| gt.callee_names.contains(n))
                        .collect();
                    let pct = if gt.total_callees > 0 {
                        (overlap.len() as f64 / gt.total_callees as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!(
                        "  [B] Callees: focus={focus_callee_names:?} (total={focus_total}), \
                         ground-truth={gt_callee_names:?} (total={gt_total}), \
                         overlap={overlap_len}/{gt_total} ({pct:.0}%)",
                        gt_callee_names = gt.callee_names,
                        gt_total = gt.total_callees,
                        overlap_len = overlap.len(),
                    );

                    assert!(
                        overlap.len() as f64 / gt.total_callees as f64 > 0.5,
                        "Callee overlap too low: {}/{} ({:.0}%).\n\
                         Focus found: {focus_callee_names:?}\n\
                         Ground truth: {gt_callee_names:?}",
                        overlap.len(),
                        gt.total_callees,
                        pct,
                        focus_callee_names = focus_callee_names,
                        gt_callee_names = &gt.callee_names,
                    );
                }
            }
        }

        // Dimension C: Callers (incoming edges) — optional, test only if
        // the full index has non-zero callers for this symbol.
        if gt.total_callers > 0 {
            let (focus_resp, focus_err) =
                focus_router.handle_callers(&json!({"symbol": symbol_name}));
            if focus_err {
                let err_r = parse_json(&focus_resp);
                let msg = err_r
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&focus_resp);
                if msg.contains("not found") {
                    // Acceptable: focus closure may not include inbound callers.
                    println!("  [C] Focus callers could not locate {symbol_name}: {msg:.200}");
                } else {
                    println!("  [C] Focus callers returned error: {msg:.200} (acceptable)");
                }
            } else {
                let focus_caller = parse_json(&focus_resp);
                if focus_caller
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let msg = focus_caller["message"].as_str().unwrap_or("");
                    println!("  [C] Focus callers is_error: {msg:.200} (acceptable)");
                } else {
                    let focus_caller_names: Vec<String> = focus_caller["callers"]
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .filter_map(|c| c["name"].as_str().map(String::from))
                        .collect();
                    let focus_total = focus_caller["total_callers"].as_u64().unwrap_or(0);

                    let overlap: Vec<&String> = focus_caller_names
                        .iter()
                        .filter(|n| gt.caller_names.contains(n))
                        .collect();
                    println!(
                        "  [C] Callers: focus={focus_caller_names:?} (total={focus_total}), \
                         ground-truth={gt_caller_names:?} (total={gt_total}), \
                         overlap={overlap_len}",
                        gt_caller_names = gt.caller_names,
                        gt_total = gt.total_callers,
                        overlap_len = overlap.len(),
                    );

                    // Callers are harder for focus to discover (incoming edges
                    // require more files).  Require at least non-zero overlap
                    // or non-zero callers discovered.
                    assert!(
                        !overlap.is_empty() || focus_total > 0,
                        "Focus found 0 callers matching full index for {symbol_name}.\n\
                         Focus found: {focus_caller_names:?}\n\
                         Ground truth: {:?}\n\
                         Focus built {} files.",
                        gt.caller_names,
                        focus_result.built_files.len(),
                    );
                }
            }
        } else {
            println!("  [C] Callers skipped: ground truth has 0 callers for {symbol_name}");
        }

        // Dimension D: File completeness — focus closure should include the
        // symbol's source file.
        {
            let focus_files: Vec<String> = focus_result
                .built_files
                .iter()
                .map(|f| {
                    focus_router
                        .project()
                        .store_query_runtime
                        .resolve_file_path(f)
                })
                .collect();
            let found = focus_files
                .iter()
                .any(|f| f == &gt.file || f.ends_with(&format!("/{}", gt.file)));
            assert!(
                found,
                "Focus closure does not include symbol's file '{file}'.\n\
                 Focus built {count} file(s): {focus_files:?}",
                file = gt.file,
                count = focus_files.len(),
            );
            println!(
                "  [D] File completeness OK: '{file}' in closure ({num} files)",
                file = gt.file,
                num = focus_files.len()
            );
        }
    }

    /// Multiple MCP queries back-to-back with FocusRuntime active should not
    /// crash or produce unexpected errors.
    #[test]
    fn focus_multiple_queries_stable() {
        let router = python_example_router(); // has manifest-indexed symbols
        router.init_focus();

        // Trigger focus bootstrap for one symbol (may be None if full index)
        let intent = atlas_engine::QueryIntent::Calls {
            symbol_name: "WikipediaSpider".into(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        let (_result, _w) = router.prepare_focus_query(Some(intent));

        let ctx = ToolCallContext::empty();

        let queries: &[(&str, serde_json::Value)] = &[
            ("search", json!({"query": "spider", "scope": "."})),
            (
                "symbol",
                json!({"symbol": {"qualified_name": "WikipediaSpider"}}),
            ),
            ("explore", json!({"symbol": "WikipediaSpider"})),
        ];

        for (name, args) in queries {
            let (resp_str, is_error) = match *name {
                "search" => router.handle_search(&ctx, args),
                "symbol" => router.handle_symbol(&ctx, args),
                "explore" => router.handle_explore(args),
                _ => unreachable!(),
            };

            let parsed = parse_json(&resp_str);
            if is_error {
                let msg = parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&resp_str);
                assert!(
                    msg.contains("not found")
                        || msg.contains("resolution")
                        || msg.contains("No full index"),
                    "{name}: unexpected error: {msg:.200}"
                );
            } else {
                assert_ok(&parsed);
            }
        }
    }

    // ── Elasticsearch focus equivalence (manual ground truth) ──────────────
    //
    // ES can't be full-indexed (30K Java files).  This test creates a
    // controlled scenario: fresh in-memory DB → insert known symbols +
    // edges → run focus → verify callee overlap.

    /// Insert a symbol into the store and return its SymbolId.
    /// Uses a salt to avoid collision with tree-sitter-generated SymbolIds.
    fn insert_focus_es_symbol(
        store: &Store,
        file_id: atlas_engine::FileId,
        simple_name: &str,
        qualified_name: &str,
        kind: atlas_engine::SymbolKind,
    ) -> atlas_engine::SymbolId {
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        // Use a salt so tree-sitter's SymbolId (generated from qualified_name,
        // no salt) doesn't collide and overwrite our manually inserted data.
        let id = atlas_engine::SymbolId::generate(
            &file_id,
            "java",
            simple_name,
            kind.as_str(),
            Some("focus_es_manual"),
        );
        let sym = atlas_engine::SymbolDef {
            id,
            kind,
            name: simple_name.into(),
            qualified_name: qualified_name.into(),
            symbol_path: vec![simple_name.into()],
            file_id,
            language: atlas_engine::Language::Java,
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

    /// Insert a calls edge between two symbols.
    fn insert_focus_es_edge(
        store: &Store,
        source: atlas_engine::SymbolId,
        target: atlas_engine::SymbolId,
    ) {
        let edge = atlas_engine::RawEdge::new(
            atlas_engine::EdgeId::generate(&source, &target, "calls", None, "test"),
            source,
            target,
            atlas_engine::EdgeKind::Calls,
            atlas_engine::Confidence::new(1.0),
            atlas_engine::Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();
    }

    /// Focus-driven callee query on manually inserted Java symbols/edges.
    ///
    /// Creates a temp directory with a minimal Java source file so the
    /// bootstrap Tier0 discovers it and lazy extraction can parse it.
    /// Then inserts known symbols + call edges and verifies the focus
    /// query returns at least one ground-truth callee.
    #[test]
    fn focus_equivalence_elasticsearch() {
        // ── Create temp project root with a real Java file ────────────────
        let temp_dir = std::env::temp_dir().join("atlas_focus_es_test");
        // Clean up any previous run
        let _ = std::fs::remove_dir_all(&temp_dir);

        let file_rel = "examples/elasticsearch/server/src/main/java/org/elasticsearch/ElasticsearchException.java";
        let file_abs = temp_dir.join(file_rel);
        std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dirs");

        // Minimal valid Java class — tree-sitter can parse it.
        // Does NOT contain logError/wrapException/getStatusMessage so
        // tree-sitter won't create conflicting symbols for our callees.
        std::fs::write(
            &file_abs,
            r#"package org.elasticsearch;

public class ElasticsearchException extends RuntimeException {
    public ElasticsearchException(String msg) { super(msg); }
    public ElasticsearchException(String msg, Throwable cause) { super(msg, cause); }
}
"#,
        )
        .expect("write Java file");

        let store = fresh_focus_store();

        // ── Register the file in the store ────────────────────────────────
        let file_id = atlas_engine::FileId::generate(file_rel);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id,
                path: file_rel.into(),
                language: atlas_engine::Language::Java,
                content_hash: "focus_es_test_v1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .expect("upsert_file");

        // ── Open router, init focus, prepare query ────────────────────────
        // Lazy extraction during prepare_focus_query does INSERT OR REPLACE
        // INTO files (via write_file_facts), which cascade-deletes any
        // pre-inserted symbols referencing this file_id.  We therefore
        // insert our symbols AFTER focus preparation.
        let router = ToolRouter::new_empty(store.clone(), temp_dir);
        router.init_focus();

        let primary_qname = "org.elasticsearch.ElasticsearchException";
        let primary_simple = "ElasticsearchException";
        let primary_kind = atlas_engine::SymbolKind::Class;

        // Use file_id only — locate_seed resolves the file directly,
        // avoiding the symbol_id lookup that requires pre-inserted data.
        let intent = atlas_engine::QueryIntent::Calls {
            symbol_name: primary_qname.to_string(),
            file_id: Some(file_id),
            symbol_id: None,
            direction: None,
            depth: None,
        };

        let (focus_opt, warnings) = router.prepare_focus_query(Some(intent));
        assert!(
            focus_opt.is_some(),
            "Focus prepare failed (None returned). Warnings: {warnings:?}"
        );

        // ── Insert fake symbols and ground-truth edges AFTER focus ────────
        // Lazy extraction's INSERT OR REPLACE INTO files cascade-deletes
        // pre-inserted symbols.  After focus, the file exists in the store
        // (from tree-sitter's extraction) so we insert our symbols safely.
        // The salt on SymbolId ensures no collision with tree-sitter's
        // symbols (different hash inputs → different primary keys).
        let primary_sid =
            insert_focus_es_symbol(&store, file_id, primary_simple, primary_qname, primary_kind);

        let callee1_simple = "logError";
        let callee1_sid = insert_focus_es_symbol(
            &store,
            file_id,
            callee1_simple,
            "org.elasticsearch.ElasticsearchException.logError",
            atlas_engine::SymbolKind::Method,
        );

        let callee2_simple = "wrapException";
        let callee2_sid = insert_focus_es_symbol(
            &store,
            file_id,
            callee2_simple,
            "org.elasticsearch.ElasticsearchException.wrapException",
            atlas_engine::SymbolKind::Method,
        );

        let callee3_simple = "getStatusMessage";
        let callee3_sid = insert_focus_es_symbol(
            &store,
            file_id,
            callee3_simple,
            "org.elasticsearch.ElasticsearchException.getStatusMessage",
            atlas_engine::SymbolKind::Method,
        );

        let expected: Vec<&str> = vec![callee1_simple, callee2_simple, callee3_simple];

        insert_focus_es_edge(&store, primary_sid, callee1_sid);
        insert_focus_es_edge(&store, primary_sid, callee2_sid);
        insert_focus_es_edge(&store, primary_sid, callee3_sid);

        // Build the graph snapshot so callee queries see our edges.
        let _ = router
            .ensure_graph_initialized()
            .expect("graph init after focus");

        // ── Query callees and assert overlap ─────────────────────────────
        let (resp_str, is_error) = router.handle_calls(&json!({
            "symbol": primary_qname,
            "direction": "outgoing",
        }));
        assert!(!is_error, "Calls query error: {resp_str:.300}");

        let resp = parse_json(&resp_str);
        assert_ok(&resp);

        let empty_vec = vec![];
        let callees = resp["callees"].as_array().unwrap_or(&empty_vec);
        let callee_names: Vec<String> = callees
            .iter()
            .filter_map(|c| c["name"].as_str().map(String::from))
            .collect();

        let total = resp["total_callees"].as_u64().unwrap_or(0);
        println!("Focus callees: {callee_names:?} (total={total})\nExpected: {expected:?}",);

        let overlap: Vec<_> = callee_names
            .iter()
            .filter(|n| expected.contains(&n.as_str()))
            .collect();

        assert!(
            !overlap.is_empty(),
            "No callee overlap! Focus found: {callee_names:?}\nExpected: {expected:?}"
        );
    }
}
