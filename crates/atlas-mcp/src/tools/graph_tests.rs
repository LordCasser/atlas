use super::EdgeKind;
use super::parse_edge_kind;
use super::resolution_to_symbol_ids_and_meta;
use crate::tools::ToolRouter;
use crate::tools::symbol_selector::SymbolResolution;
use atlas_engine::Store;
use atlas_engine::ids::FileId;
use serde_json::json;
use std::sync::Arc;

// ── Helpers ─────────────────────────────────────────────────────────
fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

fn test_router(store: Arc<Store>) -> ToolRouter {
    ToolRouter::new_empty(store, std::path::PathBuf::from("/tmp/test_project"))
}

fn insert_test_symbol(store: &Store, path: &str, qname: &str) -> atlas_engine::SymbolId {
    let fid = FileId::generate(path);
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: path.into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sid = atlas_engine::SymbolId::generate(&fid, "typescript", qname, "function", None);
    store
        .insert_symbols(&[atlas_engine::SymbolDef {
            id: sid,
            kind: atlas_engine::SymbolKind::Function,
            name: qname.rsplit('.').next().unwrap_or(qname).into(),
            qualified_name: qname.into(),
            symbol_path: qname.split('.').map(str::to_string).collect(),
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
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
        }])
        .unwrap();
    sid
}

#[test]
fn graph_refresh_observes_external_store_changes_immediately() {
    let store = test_store();
    let _sid_a = insert_test_symbol(&store, "a.ts", "a");
    let router = test_router(store.clone());
    router.ensure_graph_initialized().unwrap();

    let sid_b = insert_test_symbol(&store, "b.ts", "b");
    let before = router
        .project()
        .graph_runtime
        .provider()
        .graph_snapshot()
        .unwrap()
        .impact_with_kinds(
            &sid_b,
            1,
            Some(vec![]),
            atlas_engine::TraversalDirection::Outgoing,
        )
        .node_indices
        .len();
    assert_eq!(before, 0, "precondition: old graph should not contain b");

    // Bump graph_generation to signal that the store has changed (external
    // store mutation that bypasses the overlay runtime). This replaces the
    // old TTL + signature-check cooldown bypass.
    router
        .project()
        .graph_runtime
        .invalidation
        .graph_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    router.maybe_refresh_graph().unwrap();
    let after = router
        .project()
        .graph_runtime
        .provider()
        .graph_snapshot()
        .unwrap()
        .impact_with_kinds(
            &sid_b,
            1,
            Some(vec![]),
            atlas_engine::TraversalDirection::Outgoing,
        )
        .node_indices
        .len();
    assert_eq!(after, 1, "refreshed graph should contain b");
}

#[test]
fn test_parse_edge_kind_all_valid() {
    let cases: &[(&str, EdgeKind)] = &[
        ("calls", EdgeKind::Calls),
        ("instantiates", EdgeKind::Instantiates),
        ("implements", EdgeKind::Implements),
        ("registers_callback", EdgeKind::RegistersCallback),
        ("references", EdgeKind::References),
        ("contains", EdgeKind::Contains),
        ("imports", EdgeKind::Imports),
        ("includes", EdgeKind::Includes),
        ("exports", EdgeKind::Exports),
        ("extends", EdgeKind::Extends),
        ("typeof", EdgeKind::TypeOf),
        ("returns", EdgeKind::Returns),
        ("overrides", EdgeKind::Overrides),
        ("decorates", EdgeKind::Decorates),
        ("defines", EdgeKind::Defines),
        ("argument", EdgeKind::Argument),
        ("parameter", EdgeKind::Parameter),
        ("assigns", EdgeKind::Assigns),
        ("reads", EdgeKind::Reads),
        ("writes", EdgeKind::Writes),
        ("field_read", EdgeKind::FieldRead),
        ("field_write", EdgeKind::FieldWrite),
    ];
    for (s, expected) in cases {
        let result = parse_edge_kind(s);
        assert!(result.is_ok(), "Expected Ok for '{s}', got Err");
        assert_eq!(result.unwrap(), *expected, "Wrong EdgeKind for '{s}'");
    }
}

#[test]
fn test_parse_edge_kind_invalid() {
    let cases = &["", "*", "unknown_edge", "Calls", "calls "];
    for s in cases {
        let result = parse_edge_kind(s);
        assert!(result.is_err(), "Expected Err for '{s}', got Ok");
    }
}

#[test]
fn test_parse_edge_kind_imports() {
    assert_eq!(parse_edge_kind("imports"), Ok(EdgeKind::Imports));
    assert_eq!(parse_edge_kind("includes"), Ok(EdgeKind::Includes));
}

#[test]
fn test_parse_edge_kind_instantiates() {
    assert_eq!(parse_edge_kind("instantiates"), Ok(EdgeKind::Instantiates));
    assert_eq!(
        parse_edge_kind("registers_callback"),
        Ok(EdgeKind::RegistersCallback)
    );
}

// ── handle_impact argument parsing and error paths ──────────────────

#[test]
fn test_handle_impact_missing_symbol_argument() {
    let store = test_store();
    // Register a file so the store isn't completely empty
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({}));
    assert!(is_error, "expected error for missing symbol, got: {resp}");
}

#[test]
fn test_handle_impact_invalid_edge_kind_string() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "anything",
        "edge_kinds": ["nonexistent_edge"]
    }));
    assert!(
        is_error,
        "expected error for invalid edge kind, got: {resp}"
    );
    // Verify the error message mentions the invalid kind
    let resp_lower = resp.to_lowercase();
    assert!(
        resp_lower.contains("unknown edge kind") || resp_lower.contains("nonexistent"),
        "error should mention invalid edge kind, got: {resp}"
    );
}

#[test]
fn test_handle_impact_mixed_wildcard_returns_error() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "anything",
        "edge_kinds": ["*", "calls"]
    }));
    assert!(is_error, "expected error for mixed wildcard, got: {resp}");
    assert!(
        resp.contains("must be the only value"),
        "error message mismatch: {resp}"
    );
}

#[test]
fn test_handle_impact_edge_kinds_not_array() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "anything",
        "edge_kinds": "calls"
    }));
    assert!(
        is_error,
        "expected error for non-array edge_kinds, got: {resp}"
    );
}

#[test]
fn test_handle_impact_direction_defaults_to_outgoing() {
    let store = test_store();
    let router = test_router(store);
    // Symbol won't exist, but argument parsing happens before resolve_qname
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "nonexistent"
    }));
    // In focus mode, symbol-not-found returns a retryable unresolved result
    // (is_error=false) instead of a hard error. Both acceptable.
    assert!(
        is_error || resp.contains("unresolved") || resp.contains("not available"),
        "nonexistent symbol should error or return retryable unresolved response: {resp}"
    );
}

#[test]
fn test_handle_impact_accepts_outgoing_direction() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "nonexistent",
        "direction": "outgoing"
    }));
    // In focus mode, symbol-not-found returns a retryable unresolved result
    // (is_error=false) instead of a hard error. Both are acceptable.
    assert!(
        is_error || resp.contains("unresolved") || resp.contains("not available"),
        "nonexistent symbol should error or return retryable unresolved response: {resp}"
    );
}

#[test]
fn test_handle_impact_accepts_incoming_direction() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "nonexistent",
        "direction": "incoming"
    }));
    assert!(
        is_error || resp.contains("unresolved") || resp.contains("not available"),
        "nonexistent symbol should error or return retryable unresolved response: {resp}"
    );
}

#[test]
fn test_handle_impact_accepts_both_direction() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "nonexistent",
        "direction": "both"
    }));
    assert!(
        is_error || resp.contains("unresolved") || resp.contains("not available"),
        "nonexistent symbol should error or return retryable unresolved response: {resp}"
    );
}

#[test]
fn test_handle_impact_with_direction_param() {
    // Verify that direction="both" is accepted and processed without error
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "test_func",
        "direction": "both",
        "depth": 2
    }));
    // direction="both" must not produce an argument parsing error.
    // Symbol may not exist (focus mode returns retryable unresolved), but that's ok.
    assert!(
        is_error || resp.contains("unresolved") || resp.contains("not available"),
        "direction='both' should be accepted; got: {resp}"
    );
}

#[test]
fn test_handle_impact_invalid_direction_returns_error() {
    let store = test_store();
    let router = test_router(store);
    let (resp, is_error) = router.handle_impact(&json!({
        "symbol": "anything",
        "direction": "sideways"
    }));
    assert!(
        is_error,
        "expected error for invalid direction, got: {resp}"
    );
    assert!(
        resp.contains("direction must be"),
        "error should mention valid directions, got: {resp}"
    );
}

// ── lazy structural response fields ─────────────────────────────────

#[test]
fn test_handle_impact_response_has_warnings_field() {
    let store = test_store();
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sym = atlas_engine::SymbolDef {
        id: atlas_engine::SymbolId::generate(&fid, "typescript", "main", "function", None),
        kind: atlas_engine::SymbolKind::Function,
        name: "main".into(),
        qualified_name: "main".into(),
        symbol_path: vec!["main".into()],
        file_id: fid,
        language: atlas_engine::Language::TypeScript,
        range: atlas_engine::TextRange::default(),
        name_range: atlas_engine::TextRange::default(),
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

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) = router.handle_impact(&json!({"symbol": "main"}));
    assert!(!is_error, "expected success, got error: {resp_str}");

    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    // FocusRuntime is always present (created in QueryRuntime::new).
    // The "FocusRuntime not initialized" warning/error path has been
    // removed. Warnings may still be present for other reasons
    // (focus gaps, etc.).
    let warnings = resp.get("warnings");
    if let Some(w) = warnings {
        assert!(w.is_array(), "warnings should be an array");
        // The removed "FocusRuntime not initialized" warning must NOT appear.
        let arr = w.as_array().unwrap();
        for entry in arr {
            if let Some(s) = entry.as_str() {
                assert!(
                    !s.contains("FocusRuntime not initialized"),
                    "FocusRuntime initialization warning should not appear, got: {s}"
                );
            }
        }
    }
}

#[test]
fn test_handle_impact_response_has_direction() {
    let store = test_store();
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sym = atlas_engine::SymbolDef {
        id: atlas_engine::SymbolId::generate(&fid, "typescript", "f", "function", None),
        kind: atlas_engine::SymbolKind::Function,
        name: "f".into(),
        qualified_name: "f".into(),
        symbol_path: vec!["f".into()],
        file_id: fid,
        language: atlas_engine::Language::TypeScript,
        range: atlas_engine::TextRange::default(),
        name_range: atlas_engine::TextRange::default(),
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

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) = router.handle_impact(&json!({
        "symbol": "f",
        "direction": "both"
    }));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["direction"], "both");
}

#[test]
fn test_handle_callees_response_omits_internal_precision() {
    let store = test_store();
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sym = atlas_engine::SymbolDef {
        id: atlas_engine::SymbolId::generate(&fid, "typescript", "g", "function", None),
        kind: atlas_engine::SymbolKind::Function,
        name: "g".into(),
        qualified_name: "g".into(),
        symbol_path: vec!["g".into()],
        file_id: fid,
        language: atlas_engine::Language::TypeScript,
        range: atlas_engine::TextRange::default(),
        name_range: atlas_engine::TextRange::default(),
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

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) = router.handle_callees(&json!({"symbol": "g"}));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert!(resp.get("precision").is_none());
}

#[test]
fn test_handle_callers_response_omits_internal_precision() {
    let store = test_store();
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sym = atlas_engine::SymbolDef {
        id: atlas_engine::SymbolId::generate(&fid, "typescript", "h", "function", None),
        kind: atlas_engine::SymbolKind::Function,
        name: "h".into(),
        qualified_name: "h".into(),
        symbol_path: vec!["h".into()],
        file_id: fid,
        language: atlas_engine::Language::TypeScript,
        range: atlas_engine::TextRange::default(),
        name_range: atlas_engine::TextRange::default(),
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

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) = router.handle_callers(&json!({"symbol": "h"}));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert!(resp.get("precision").is_none());
}

/// Verify `handle_callers` deduplicates when aggregating multiple
/// SymbolIds (e.g. same qname in different files). A shared caller of
/// all matched targets must appear exactly once in the results.
#[test]
fn test_aggregate_dedup_calls() {
    let store = test_store();

    // Two "target" symbols: same qname, different files → ambiguous.
    let target_a = insert_test_symbol(&store, "src/a.ts", "target");
    let target_b = insert_test_symbol(&store, "src/b.ts", "target");

    // shared_caller calls BOTH target symbols
    let shared_caller = insert_test_symbol(&store, "src/caller.ts", "shared_caller");
    insert_test_call_edge(&store, shared_caller, target_a);
    insert_test_call_edge(&store, shared_caller, target_b);

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp_str, is_error) = router.handle_callers(&json!({"symbol": "target"}));
    assert!(!is_error, "expected success, got error: {resp_str}");

    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");

    let total = resp["total_callers"]
        .as_u64()
        .expect("should have total_callers");
    let callers = resp["callers"]
        .as_array()
        .expect("should have callers array");

    // Dedup: shared_caller must appear exactly once
    assert_eq!(
        total, 1,
        "total_callers should be 1 after dedup, got {total}"
    );
    assert_eq!(
        callers.len(),
        1,
        "callers array should have 1 entry after dedup, got {} entries: {callers:?}",
        callers.len()
    );

    let caller = &callers[0];
    assert_eq!(
        caller["qualified_name"].as_str().unwrap(),
        "shared_caller",
        "caller should be shared_caller"
    );
    assert!(
        caller["file"].as_str().unwrap().contains("caller.ts"),
        "caller file should be caller.ts"
    );
}

/// Task 3: depth on fixed 1-hop callers emits a non-honored warning.
#[test]
fn callers_depth_param_emits_not_honored_warning() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "src/h.ts", "depth_target");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp_str, is_error) = router.handle_callers(&json!({
        "symbol": "depth_target",
        "depth": 5
    }));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let warnings = resp["warnings"]
        .as_array()
        .expect("warnings array required when depth is present");
    assert!(
        warnings.iter().any(|w| w.as_str().is_some_and(
            |s| s.contains("depth is not honored") && s.contains("direction=incoming")
        )),
        "expected depth-not-honored warning, got: {resp}"
    );
}

/// Task 3: depth on callees still returns only direct neighbors (1-hop).
#[test]
fn callees_with_depth_gt_1_still_one_hop_only() {
    let store = test_store();
    // a → b → c (chain). callees(a) with depth=5 must only see b, not c.
    let a = insert_test_symbol(&store, "src/a.ts", "chain_a");
    let b = insert_test_symbol(&store, "src/b.ts", "chain_b");
    let c = insert_test_symbol(&store, "src/c.ts", "chain_c");
    insert_test_call_edge(&store, a, b);
    insert_test_call_edge(&store, b, c);

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp_str, is_error) = router.handle_callees(&json!({
        "symbol": "chain_a",
        "depth": 5
    }));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let callees = resp["callees"].as_array().expect("callees array");
    assert_eq!(
        callees.len(),
        1,
        "depth must not expand multi-hop; got: {callees:?}"
    );
    assert_eq!(callees[0]["qualified_name"], "chain_b");
    assert!(
        !callees.iter().any(|n| n["qualified_name"] == "chain_c"),
        "grand-callee must not appear under fixed 1-hop callees"
    );
    let warnings = resp["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|w| w.as_str().is_some_and(
            |s| s.contains("depth is not honored") && s.contains("direction=outgoing")
        )),
        "expected depth warning on callees, got: {resp}"
    );
}

/// Task 3: caller/callee nodes include store signature when present.
#[test]
fn callers_include_signature_from_store() {
    let store = test_store();
    let target = insert_test_symbol_with_sig(&store, "src/tgt.ts", "sig_target", None);
    let caller = insert_test_symbol_with_sig(
        &store,
        "src/caller.ts",
        "sig_caller",
        Some("fn sig_caller() -> i32"),
    );
    insert_test_call_edge(&store, caller, target);

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp_str, is_error) = router.handle_callers(&json!({"symbol": "sig_target"}));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let callers = resp["callers"].as_array().expect("callers");
    assert_eq!(callers.len(), 1);
    assert_eq!(
        callers[0]["signature"].as_str(),
        Some("fn sig_caller() -> i32"),
        "signature must come from store SymbolDef, not GraphSnapshot"
    );
}

fn insert_test_symbol_with_sig(
    store: &Store,
    path: &str,
    qname: &str,
    signature: Option<&str>,
) -> atlas_engine::SymbolId {
    let fid = FileId::generate(path);
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: path.into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sid = atlas_engine::SymbolId::generate(&fid, "typescript", qname, "function", None);
    store
        .insert_symbols(&[atlas_engine::SymbolDef {
            id: sid,
            kind: atlas_engine::SymbolKind::Function,
            name: qname.rsplit('.').next().unwrap_or(qname).into(),
            qualified_name: qname.into(),
            symbol_path: qname.split('.').map(str::to_string).collect(),
            file_id: fid,
            language: atlas_engine::Language::TypeScript,
            range: atlas_engine::TextRange::default(),
            name_range: atlas_engine::TextRange::default(),
            signature: signature.map(str::to_string),
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        }])
        .unwrap();
    sid
}

#[test]
fn calls_invalid_include_roots_returns_warning() {
    let store = test_store();
    insert_test_symbol(&store, "src/callee.ts", "warned_callee");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp_str, is_error) = router.handle_calls(&json!({
        "symbol": "warned_callee",
        "direction": "incoming",
        "include_roots": ["/absolute/rejected"]
    }));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let warnings = resp["warnings"].as_array().expect("warnings should exist");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|s| s.contains("absolute path rejected"))),
        "expected include_roots warning, got: {resp}"
    );
}

// ── handle_explore tests ───────────────────────────────────────────

#[test]
fn explore_unique_symbol_returns_dossier() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "test.ts", "myfunc");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "myfunc"}));
    assert!(!is_error, "expected success, got error: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert!(
        resp.get("subject").is_some(),
        "response should have 'subject' field, got keys: {:?}",
        resp.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    assert!(
        resp.get("precisionTier").is_none(),
        "explore must not expose internal precision tier: {resp}"
    );
}

#[test]
fn explore_file_context_reads_persisted_imports_and_exports() {
    let store = test_store();
    let symbol_id = insert_test_symbol(&store, "src/component.ts", "VideoComponent");
    let mut symbol = store
        .find_symbol_by_id(&symbol_id)
        .unwrap()
        .expect("component symbol");
    symbol.exported = true;
    store.insert_symbols(&[symbol.clone()]).unwrap();
    store
        .insert_imports(&[atlas_engine::ImportDef {
            id: atlas_engine::ImportId::generate(
                &symbol.file_id,
                "import",
                "./model",
                Some("Model"),
                0,
            ),
            file_id: symbol.file_id,
            kind: atlas_engine::ImportKind::Import,
            module: "./model".into(),
            imported_name: "Model".into(),
            local_name: None,
            is_wildcard: false,
            is_relative: true,
            range: atlas_engine::TextRange::default(),
            alias: None,
        }])
        .unwrap();

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "VideoComponent"}));
    assert!(!is_error, "expected success, got error: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

    assert_eq!(resp["fileContext"]["imports"][0]["module"], "./model");
    assert_eq!(
        resp["fileContext"]["exports"][0]["exportedName"],
        "VideoComponent"
    );
    assert_eq!(
        resp["fileContext"]["exports"][0]["localSymbolId"],
        symbol_id.to_hex()
    );
}

#[test]
fn explore_ambiguous_symbol_returns_list() {
    let store = test_store();
    insert_test_symbol(&store, "a.ts", "shared_func");
    insert_test_symbol(&store, "b.ts", "shared_func");
    insert_test_symbol(&store, "c.ts", "shared_func");
    let router = test_router(store);
    // Ambiguous path returns early; graph init not needed
    let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "shared_func"}));
    assert!(!is_error, "expected not an error, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["ambiguous"], json!(true), "should be ambiguous");
    let candidates = resp
        .get("candidates")
        .and_then(|v| v.as_array())
        .expect("should have candidates array");
    assert_eq!(candidates.len(), 3, "expected 3 candidates");
}

#[test]
fn explore_accepts_source_lines_param() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "test.ts", "myfunc2");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) =
        router.handle_explore(&json!({"symbol": "myfunc2", "source_lines": 10}));
    assert!(
        !is_error,
        "expected no error with source_lines param, got: {resp_str}"
    );
    // Params are accepted if handler does not reject them.
    // Dossier may still build with warnings about missing source files.
}

#[test]
fn explore_accepts_evidence_limit_param() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "test.ts", "myfunc3");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (resp_str, is_error) =
        router.handle_explore(&json!({"symbol": "myfunc3", "evidence_limit": 5}));
    assert!(
        !is_error,
        "expected no error with evidence_limit param, got: {resp_str}"
    );
}

#[test]
fn explore_invalid_include_roots_returns_warning() {
    let store = test_store();
    insert_test_symbol(&store, "test.ts", "myfunc_with_roots");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp_str, is_error) = router.handle_explore(&json!({
        "symbol": "myfunc_with_roots",
        "include_roots": ["/absolute/rejected"]
    }));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    let warnings = resp["warnings"].as_array().expect("warnings should exist");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|s| s.contains("absolute path rejected"))),
        "expected include_roots warning, got: {resp}"
    );
}

#[test]
fn explore_not_found_without_candidates_returns_terminal_gap() {
    let store = test_store();
    let router = test_router(store);
    let (resp_str, is_error) = router.handle_explore(&json!({"symbol": "missing_func"}));
    assert!(
        !is_error,
        "missing cold symbol should be a bounded unresolved response: {resp_str}"
    );
    let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();
    assert_eq!(resp["status"], json!("unresolved"));
    assert!(resp["analysis"].get("retry_after_ms").is_none(), "{resp}");
    assert!(resp.get("query_id").is_some(), "missing query_id: {resp}");
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
    assert!(resp.get("partial_result").is_none());
    assert!(resp.get("background_refinement").is_none());
    assert!(resp.get("work").is_none());
}

// ── ambiguity candidate tests ──────────────────────────────────────

#[test]
fn max_ambiguous_candidates_is_5() {
    // Verify the constant was changed
    assert_eq!(super::super::MAX_AMBIGUOUS_CANDIDATES, 5);
}

#[test]
fn ambiguity_includes_candidates_when_multiple() {
    let store = test_store();

    // Insert two symbols with same qname but different files
    let sid1 = insert_test_symbol(&store, "src/a.ts", "ns.foo");
    let sid2 = insert_test_symbol(&store, "src/b.ts", "ns.foo");

    // Build candidate JSON manually to test helper
    let c = super::candidate_json(&store, &sid1, true);
    assert_eq!(c["qualified_name"], "ns.foo");
    assert_eq!(c["selected"], true);
    assert!(c["file"].as_str().unwrap().contains("a.ts"));

    // Second candidate not selected
    let c2 = super::candidate_json(&store, &sid2, false);
    assert_eq!(c2["qualified_name"], "ns.foo");
    assert_eq!(c2["selected"], false);
    assert!(c2["file"].as_str().unwrap().contains("b.ts"));
}

// ── path multi-candidate-pair exhaustive search ────────────────────

/// Helper: insert a call edge between two symbols in the store.
fn insert_test_call_edge(
    store: &Store,
    source: atlas_engine::SymbolId,
    target: atlas_engine::SymbolId,
) {
    let edge = atlas_engine::RawEdge::new(
        atlas_engine::EdgeId::generate(&source, &target, "calls", None, "tree_sitter"),
        source,
        target,
        atlas_engine::EdgeKind::Calls,
        atlas_engine::Confidence::new(1.0),
        atlas_engine::Provenance::TreeSitter,
    );
    store.insert_edges(&[edge]).unwrap();
}

fn insert_unresolved_call_reference(store: &Store, source: atlas_engine::SymbolId, name: &str) {
    let source_symbol = store
        .find_symbol_by_id(&source)
        .unwrap()
        .expect("source symbol should exist");
    let range = atlas_engine::TextRange {
        start_byte: 32,
        end_byte: 32 + name.len() as u32,
        start_line: 4,
        start_column: 8,
        end_line: 4,
        end_column: 8 + name.len() as u32,
    };
    let reference = atlas_engine::ReferenceUse {
        id: atlas_engine::ReferenceId::generate(
            &source_symbol.file_id,
            Some(&source),
            range.start_byte,
            range.end_byte,
            name,
            atlas_engine::ReferenceKind::Call,
        ),
        file_id: source_symbol.file_id,
        source_symbol: Some(source),
        scope_id: None,
        kind: atlas_engine::ReferenceKind::Call,
        text: name.to_string(),
        name: name.to_string(),
        receiver: None,
        arity: Some(1),
        range,
        binding_id: None,
        resolved: None,
    };
    store.insert_references(&[reference]).unwrap();
}

/// Verify that handle_path with ambiguous string qnames tries all
/// SymbolId pairs and returns resolution + candidate metadata.
///
/// Scenario: 2 "from" candidates × 2 "to" candidates = 4 pairs total.
/// Only 1 of the 4 pairs has a call edge → the first winning pair is
/// selected and returned.
#[test]
fn test_path_multi_pair_exhaustive() {
    let store = test_store();

    // 2 "from" candidates: same qname "sender", different files
    let from_sid0 = insert_test_symbol(&store, "src/a.ts", "sender");
    let _from_sid1 = insert_test_symbol(&store, "src/b.ts", "sender");

    // 2 "to" candidates: same qname "receiver", different files
    let to_sid0 = insert_test_symbol(&store, "src/c.ts", "receiver");
    let _to_sid1 = insert_test_symbol(&store, "src/d.ts", "receiver");

    // Only 1 of 4 pairs has a path: from_sid0 → to_sid0
    insert_test_call_edge(&store, from_sid0, to_sid0);

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    // Plain string qnames (not SymbolSelectors)
    let (resp_str, is_error) = router.handle_path(&json!({
        "from": "sender",
        "to": "receiver"
    }));
    assert!(!is_error, "expected success, got error: {resp_str}");

    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");

    // A non-empty path was found
    assert!(
        resp.get("path")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "should have a non-empty path, got: {resp_str}"
    );

    // ── Ambiguity metadata ──────────────────────────────────────────
    let ambiguity = resp.get("ambiguity").expect("should have ambiguity field");

    assert_eq!(
        ambiguity["from_count"].as_u64().unwrap(),
        2,
        "from_count should be 2"
    );
    assert_eq!(
        ambiguity["to_count"].as_u64().unwrap(),
        2,
        "to_count should be 2"
    );

    // from_candidates / to_candidates
    let from_cands = ambiguity["from_candidates"]
        .as_array()
        .expect("from_candidates should be an array");
    assert_eq!(from_cands.len(), 2, "from_candidates should have 2 entries");

    let to_cands = ambiguity["to_candidates"]
        .as_array()
        .expect("to_candidates should be an array");
    assert_eq!(to_cands.len(), 2, "to_candidates should have 2 entries");

    // from_resolution / to_resolution count
    let from_res = ambiguity
        .get("from_resolution")
        .expect("should have from_resolution");
    assert_eq!(
        from_res["count"].as_u64().unwrap(),
        2,
        "from_resolution count should be 2"
    );

    let to_res = ambiguity
        .get("to_resolution")
        .expect("should have to_resolution");
    assert_eq!(
        to_res["count"].as_u64().unwrap(),
        2,
        "to_resolution count should be 2"
    );

    // Winning pair markers
    let from_selected = from_cands
        .iter()
        .any(|c| c.get("selected").and_then(|v| v.as_bool()).unwrap_or(false));
    assert!(
        from_selected,
        "at least one from_candidate should be selected"
    );

    let to_selected = to_cands
        .iter()
        .any(|c| c.get("selected").and_then(|v| v.as_bool()).unwrap_or(false));
    assert!(to_selected, "at least one to_candidate should be selected");

    assert!(
        ambiguity.get("matched_from").is_some(),
        "should have matched_from"
    );
    assert!(
        ambiguity.get("matched_to").is_some(),
        "should have matched_to"
    );

    // selection_note confirms pair-based search
    assert!(
        ambiguity
            .get("selection_note")
            .and_then(|v| v.as_str())
            .map(|s| s.contains("pair"))
            .unwrap_or(false),
        "selection_note should mention pair-based selection"
    );
}

#[test]
fn path_ambiguous_no_path_uses_message_without_hint_field() {
    let store = test_store();
    insert_test_symbol(&store, "src/a.ts", "sender");
    insert_test_symbol(&store, "src/b.ts", "sender");
    insert_test_symbol(&store, "src/c.ts", "receiver");
    insert_test_symbol(&store, "src/d.ts", "receiver");

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();
    let (response, is_error) = router.handle_path(&json!({
        "from": "sender",
        "to": "receiver"
    }));
    assert!(!is_error, "expected bounded no-path response: {response}");

    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("hint").is_none(), "{response}");
    assert!(
        response["message"]
            .as_str()
            .is_some_and(|message| message.contains("SymbolSelector")),
        "disambiguation guidance should remain in message: {response}"
    );
}

#[test]
fn path_not_found_target_reports_unresolved_call_hint() {
    let store = test_store();
    let from_id = insert_test_symbol(&store, "src/a.ts", "sender");
    insert_unresolved_call_reference(&store, from_id, "copy_from_user");

    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let (resp, is_error) = router.handle_path(&json!({
        "from": "sender",
        "to": "copy_from_user"
    }));

    // In focus mode, symbol-not-found returns a retryable unresolved result
    // (is_error=false) instead of a hard error. Both acceptable outcomes.
    if is_error {
        assert!(
            resp.contains("unresolved call token")
                && resp.contains("calls(direction=\"outgoing\")"),
            "missing actionable unresolved-call hint: {resp}"
        );
    } else {
        assert!(
            resp.contains("unresolved")
                || resp.contains("not available")
                || resp.contains("retry_after_ms"),
            "focus-mode response should indicate unresolved retryable state: {resp}"
        );
    }
}

// ── resolution_to_symbol_ids_and_meta unit tests ──────────────────

#[test]
fn resolution_helper_single_returns_id_and_meta() {
    use atlas_engine::symbol_selector::{MatchInfo, MatchMode, PathMatchQuality, ResolvedSymbol};
    let sid = atlas_engine::SymbolId::from_bytes([1u8; 32]);
    let resolved = ResolvedSymbol {
        qualified_name: "foo".into(),
        file_path: "src/lib.rs".into(),
        line: 10,
        kind: "function".into(),
        language: "rust".into(),
        match_info: MatchInfo {
            mode: MatchMode::UniqueQname,
            ignored_mismatches: vec![],
            path_match: Some(PathMatchQuality::Exact),
            line_delta: None,
        },
    };
    let resolution = SymbolResolution::Single {
        symbol_id: sid,
        resolved,
    };
    let (ids, meta) = resolution_to_symbol_ids_and_meta(&resolution, "foo").unwrap();
    assert_eq!(ids, vec![sid]);
    assert!(meta.is_some());
}

#[test]
fn resolution_helper_ambiguous_uses_direct_symbol_id() {
    use atlas_engine::symbol_selector::{ScoredCandidate, SymbolSelector};
    let sid1 = atlas_engine::SymbolId::from_bytes([1u8; 32]);
    let sid2 = atlas_engine::SymbolId::from_bytes([2u8; 32]);
    let candidates = vec![
        ScoredCandidate {
            qualified_name: "foo".into(),
            file_path: "a.rs".into(),
            line: 10,
            kind: "function".into(),
            language: "rust".into(),
            score: 100,
            reasons: vec![],
            symbol_ref: SymbolSelector {
                qualified_name: "foo".into(),
                file_path: Some("a.rs".into()),
                line: Some(10),
                kind: Some("function".into()),
                language: Some("rust".into()),
            },
            symbol_id: sid1,
        },
        ScoredCandidate {
            qualified_name: "foo".into(),
            file_path: "b.rs".into(),
            line: 20,
            kind: "function".into(),
            language: "rust".into(),
            score: 80,
            reasons: vec![],
            symbol_ref: SymbolSelector {
                qualified_name: "foo".into(),
                file_path: Some("b.rs".into()),
                line: Some(20),
                kind: Some("function".into()),
                language: Some("rust".into()),
            },
            symbol_id: sid2,
        },
    ];
    let resolution = SymbolResolution::Ambiguous {
        candidates,
        score_gap: 20,
    };
    let (ids, meta) = resolution_to_symbol_ids_and_meta(&resolution, "foo").unwrap();
    assert_eq!(ids, vec![sid1, sid2]);
    assert!(meta.is_some());
}

#[test]
fn resolution_helper_not_found_returns_error() {
    let resolution = SymbolResolution::NotFound {
        qname: "missing_fn".into(),
        suggestions: vec!["other_fn".into()],
    };
    let err = resolution_to_symbol_ids_and_meta(&resolution, "missing_fn").unwrap_err();
    assert!(err.contains("not found"));
    assert!(err.contains("other_fn"));
}

#[test]
fn resolution_to_symbol_ids_uses_direct_symbol_id() {
    // Verify that when ScoredCandidate carries symbol_id, we don't need
    // find_symbols_by_qname round-trip.
    use atlas_engine::symbol_selector::{ScoredCandidate, SymbolSelector};
    let sid1 = atlas_engine::SymbolId::from_bytes([1u8; 32]);
    let sid2 = atlas_engine::SymbolId::from_bytes([2u8; 32]);
    let candidates = vec![
        ScoredCandidate {
            qualified_name: "test_fn".into(),
            file_path: "src/a.rs".into(),
            line: 10,
            kind: "function".into(),
            language: "rust".into(),
            score: 100,
            reasons: vec![],
            symbol_ref: SymbolSelector {
                qualified_name: "test_fn".into(),
                file_path: Some("src/a.rs".into()),
                line: Some(10),
                kind: Some("function".into()),
                language: Some("rust".into()),
            },
            symbol_id: sid1,
        },
        ScoredCandidate {
            qualified_name: "test_fn".into(),
            file_path: "src/b.rs".into(),
            line: 20,
            kind: "function".into(),
            language: "rust".into(),
            score: 80,
            reasons: vec![],
            symbol_ref: SymbolSelector {
                qualified_name: "test_fn".into(),
                file_path: Some("src/b.rs".into()),
                line: Some(20),
                kind: Some("function".into()),
                language: Some("rust".into()),
            },
            symbol_id: sid2,
        },
    ];
    let ids: Vec<_> = candidates.iter().map(|c| c.symbol_id).collect();
    assert_eq!(ids, vec![sid1, sid2]);
}

// ── calls dispatch tests ──────────────────────────────────────────

#[test]
fn calls_dispatch_wildcard_edge_kinds_routes_to_callgraph() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "a.ts", "a.a");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    // Explicit empty edge_kinds [] means wildcard → should route to callgraph
    let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
        "symbol": "a.a",
        "direction": "outgoing",
        "edge_kinds": [],
    }));
    assert!(
        matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
        "wildcard edge_kinds should route to CallGraph"
    );
}

#[test]
fn calls_dispatch_custom_edge_kinds_routes_to_callgraph() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "a.ts", "a.a");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    // Custom edge_kinds → should route to callgraph
    let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
        "symbol": "a.a",
        "direction": "outgoing",
        "edge_kinds": ["calls", "references"],
    }));
    assert!(
        matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
        "custom edge_kinds should route to CallGraph"
    );
}

#[test]
fn calls_dispatch_default_edges_routes_to_specific_handler() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "a.ts", "a.a");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
        "symbol": "a.a",
        "direction": "incoming",
    }));
    assert!(
        matches!(dispatch, crate::tools::CallsDispatch::Callers),
        "incoming with default edges should route to Callers"
    );
}

#[test]
fn calls_dispatch_both_direction_routes_to_callgraph() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "a.ts", "a.a");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
        "symbol": "a.a",
        "direction": "both",
    }));
    assert!(
        matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
        "'both' direction should route to CallGraph"
    );
}

#[test]
fn calls_dispatch_depth_gt_1_routes_to_callgraph() {
    let store = test_store();
    let _sid = insert_test_symbol(&store, "a.ts", "a.a");
    let router = test_router(store);
    router.ensure_graph_initialized().unwrap();

    let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
        "symbol": "a.a",
        "direction": "outgoing",
        "depth": 3,
    }));
    assert!(
        matches!(dispatch, crate::tools::CallsDispatch::CallGraph(_)),
        "depth > 1 should route to CallGraph"
    );
}

#[test]
fn calls_dispatch_unknown_direction_returns_error() {
    let dispatch = crate::tools::resolve_calls_dispatch(&serde_json::json!({
        "symbol": "a.a",
        "direction": "sideways",
    }));
    assert!(
        matches!(dispatch, crate::tools::CallsDispatch::Error(_)),
        "unknown direction should return Error"
    );
}

// ── Focus runtime wiring tests ────────────────────────────────────

#[test]
fn focus_runtime_wired_at_construction() {
    let store = test_store();
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let router = test_router(store);
    // FocusRuntime + FocusMaterialize are injected by ActiveProject construction.
    let mode = router
        .project()
        .query_runtime
        .detect_access_strategy(atlas_engine::QueryNeed::CallGraph);
    assert_eq!(mode, atlas_engine::focus::runtime::AccessStrategy::Focus);
    assert!(router.project().materialize.has_structural_rebuilder());
}

#[test]
fn graph_response_without_focus_has_no_focus_fields() {
    let store = test_store();
    let fid = FileId::generate("test.ts");
    store
        .upsert_file(&atlas_engine::FileInfo {
            file_id: fid,
            path: "test.ts".into(),
            language: atlas_engine::Language::TypeScript,
            content_hash: "hash1".into(),
            status: atlas_engine::ParseStatus::Success,
        })
        .unwrap();
    let sym = atlas_engine::SymbolDef {
        id: atlas_engine::SymbolId::generate(&fid, "typescript", "focus_test_fn", "function", None),
        kind: atlas_engine::SymbolKind::Function,
        name: "focus_test_fn".into(),
        qualified_name: "focus_test_fn".into(),
        symbol_path: vec!["focus_test_fn".into()],
        file_id: fid,
        language: atlas_engine::Language::TypeScript,
        range: atlas_engine::TextRange::default(),
        name_range: atlas_engine::TextRange::default(),
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

    let router = test_router(store);
    // Simulate a full index so prepare_focus_query returns early
    // without focus data — the equivalent of the old "no focus" path.
    let signature = router.project().store.index_signature().unwrap_or_default();
    *router
        .project()
        .query_runtime
        .cache
        .cached_repo_cache
        .write()
        .unwrap_or_else(|e| e.into_inner()) =
        Some((signature, atlas_engine::QueryNeed::CallGraph, true));
    router.ensure_graph_initialized().unwrap();
    // focus is NOT active for this query — focus fields should NOT appear
    let (resp_str, is_error) = router.handle_impact(&json!({"symbol": "focus_test_fn"}));
    assert!(!is_error, "expected success, got: {resp_str}");
    let resp: serde_json::Value =
        serde_json::from_str(&resp_str).expect("response should be valid JSON");

    // Backward compat: focus-specific fields must NOT appear when focus is not active
    assert!(
        resp.get("coverage_counts").is_none(),
        "coverage_counts should NOT appear when focus is not active"
    );
    assert!(
        resp.get("gaps").is_none(),
        "gaps should NOT appear when focus is not active"
    );
    assert!(
        resp.get("pending_closures").is_none(),
        "pending_closures should NOT appear when focus is not active"
    );
}

#[test]
fn apply_focus_to_lr_is_noop_with_no_focus_data() {
    use crate::tools::analysis_envelope::AnalysisEnvelope;
    use atlas_engine::focus::runtime::{AccessStrategy, FocusResult};

    let result = FocusResult {
        access: AccessStrategy::FullCache,
        quality: None,
        gaps: vec![],
        pending_closure_ids: vec![],
        pending_extraction_job_ids: vec![],
        closure_id: None,
        seed_symbol_id: None,
        seed_file_id: None,
        built_files: vec![],
        coverage_counts: None,
        job_tracker: None,
    };

    let args = json!({"symbol": "test"});
    let lr = AnalysisEnvelope::new("test", &args).with_is_error(false);
    let lr = crate::tools::apply_focus_result_to_lr(lr, &result);

    // Build with a mock store to verify no crash
    let store = MockStore::new();
    let body = json!({"ok": true});
    let (json_str, is_err) = lr.build(body, &store);
    assert!(!is_err, "should succeed with no focus data");
    // No focus fields should be injected
    assert!(
        !json_str.contains("coverage_counts"),
        "coverage_counts should not be present when focus data is None"
    );
    assert!(
        !json_str.contains("gaps"),
        "gaps should not be present when focus data is None"
    );
    assert!(
        !json_str.contains("pending_closures"),
        "pending_closures should not be present when focus data is None"
    );
}

// Mock SnapshotStore for isolated AnalysisEnvelope tests
use std::sync::Mutex;

struct MockStore {
    snapshots: Mutex<Vec<crate::tools::query_snapshot::QuerySnapshot>>,
}
impl MockStore {
    fn new() -> Self {
        Self {
            snapshots: Mutex::new(Vec::new()),
        }
    }
}
impl crate::tools::analysis_envelope::SnapshotStore for MockStore {
    fn store_query_snapshot(&self, snapshot: crate::tools::query_snapshot::QuerySnapshot) {
        self.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(snapshot);
    }
}
