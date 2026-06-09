//! Integration tests for the SymbolSelector system.
//!
//! Covers:
//! - Schema `oneOf` compatibility across all MCP tools
//! - Closed-loop symbol_ref round-trips (search → calls, symbol → impact, etc.)
//! - Auto-aggregation behaviour for calls / impact / path
//! - Fault-tolerance: wrong file_path / line / kind never block resolution
//! - Hex ID abolition: no raw SymbolId hex in serialized output; hex input gracefully rejected
//!
//! ```bash
//! cargo test -p atlas-mcp --test symbol_selector_integration
//! ```

use atlas_engine::symbol_selector::{
    MatchMode, ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
    SymbolSelector, resolve_symbol_input,
};
use atlas_engine::{
    FileId, FileInfo, Language, ParseStatus, Store, SymbolDef, SymbolId, SymbolKind, TextRange,
};

// ===========================================================================
// Helpers
// ===========================================================================

/// Create an in-memory Store with schema initialised.
fn test_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    store
}

/// Register a dummy file in the store and return its FileId.
fn register_file(store: &Store, path: &str, language: Language) -> FileId {
    let file_id = FileId::generate(path);
    store
        .upsert_file(&FileInfo {
            file_id,
            path: path.to_string(),
            language,
            content_hash: "hash1".to_string(),
            status: ParseStatus::Success,
        })
        .unwrap();
    file_id
}

/// Create a minimal SymbolDef for testing.
fn make_symbol(
    file_id: FileId,
    language: Language,
    kind: SymbolKind,
    qname: &str,
    name: &str,
    line: u32, // 1-based
) -> SymbolDef {
    let line0 = line.saturating_sub(1); // 0-based
    let range = TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: line0,
        start_column: 1,
        end_line: line0,
        end_column: 11,
    };
    SymbolDef {
        id: SymbolId::generate(&file_id, language.as_str(), qname, kind.as_str(), None),
        kind,
        name: name.to_string(),
        qualified_name: qname.to_string(),
        symbol_path: qname.split(&['.', ':']).map(String::from).collect(),
        file_id,
        language,
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
        layer: "structural".to_string(),
    }
}

/// Insert symbols into the store.
fn insert_symbols(store: &Store, symbols: &[SymbolDef]) {
    store.insert_symbols(symbols).unwrap();
}

// ===========================================================================
// Schema oneOf tests (verify make_all_tools schemas)
// ===========================================================================

/// Helper: find a tool by name in the tool list.
fn find_tool<'a>(tools: &'a [atlas_mcp::Tool], name: &str) -> &'a atlas_mcp::Tool {
    tools
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("tool '{name}' not found in make_all_tools()"))
}

/// Helper: extract the `oneOf` array from a schema property named `prop_name`.
fn get_oneof_for_param<'a>(tool: &'a atlas_mcp::Tool, prop_name: &str) -> &'a serde_json::Value {
    let props = tool
        .input_schema
        .properties
        .as_ref()
        .expect("schema must have properties");
    let param = props
        .get(prop_name)
        .unwrap_or_else(|| panic!("{prop_name} not found in schema for {}", tool.name));
    param
        .get("oneOf")
        .unwrap_or_else(|| panic!("{prop_name} missing oneOf in schema for {}", tool.name))
}

/// Helper: assert that a property's oneOf array has exactly 2 entries
/// (plain string + SymbolSelector object), and the second is the object variant.
fn assert_oneof_has_string_and_object(oneof: &serde_json::Value) {
    let arr = oneof
        .as_array()
        .expect("oneOf must be an array");
    assert!(
        arr.len() >= 2,
        "oneOf must have at least 2 entries, got {arr:?}"
    );

    // First entry: plain string
    let string_entry = &arr[0];
    assert_eq!(
        string_entry.get("type").and_then(|v| v.as_str()),
        Some("string"),
        "first oneOf entry should be string type, got {string_entry:?}"
    );

    // Second entry: SymbolSelector object
    let obj_entry = &arr[1];
    assert_eq!(
        obj_entry.get("type").and_then(|v| v.as_str()),
        Some("object"),
        "second oneOf entry should be SymbolSelector object, got {obj_entry:?}"
    );
    assert_eq!(
        obj_entry
            .get("required")
            .and_then(|v| v.as_array())
            .map(|r| r.iter().any(|v| v.as_str() == Some("qualified_name"))),
        Some(true),
        "SymbolSelector must require 'qualified_name'"
    );
}

#[test]
fn test_calls_schema_has_oneof() {
    let tools = atlas_mcp::make_all_tools();
    let calls = find_tool(&tools, "calls");
    let oneof = get_oneof_for_param(calls, "symbol");
    assert_oneof_has_string_and_object(oneof);
}

#[test]
fn test_impact_schema_has_oneof() {
    let tools = atlas_mcp::make_all_tools();
    let impact = find_tool(&tools, "impact");
    let oneof = get_oneof_for_param(impact, "symbol");
    assert_oneof_has_string_and_object(oneof);
}

#[test]
fn test_symbol_schema_has_oneof() {
    let tools = atlas_mcp::make_all_tools();
    let symbol = find_tool(&tools, "symbol");
    let oneof = get_oneof_for_param(symbol, "symbol");
    assert_oneof_has_string_and_object(oneof);
}

#[test]
fn test_path_schema_has_oneof() {
    let tools = atlas_mcp::make_all_tools();
    let path = find_tool(&tools, "path");

    // Both 'from' and 'to' must have oneOf
    for param_name in &["from", "to"] {
        let oneof = get_oneof_for_param(path, param_name);
        assert_oneof_has_string_and_object(oneof);
    }
}

#[test]
fn test_trace_schema_has_oneof() {
    let tools = atlas_mcp::make_all_tools();
    let trace = find_tool(&tools, "trace");

    // 'symbol', 'from', 'to' all have oneOf
    for param_name in &["symbol", "from", "to"] {
        let oneof = get_oneof_for_param(trace, param_name);
        assert_oneof_has_string_and_object(oneof);
    }
}

// ===========================================================================
// Resolution / round-trip / aggregation / fault-tolerance tests
// ===========================================================================

/// Helper: resolve with UniqueOrCandidates policy, expect a Single result.
fn resolve_single(store: &Store, input: &SymbolInput) -> SymbolResolution {
    let res = resolve_symbol_input(store, input, SymbolResolutionPolicy::UniqueOrCandidates)
        .expect("resolution should succeed");
    match res {
        SymbolResolution::Single { .. } => res,
        SymbolResolution::Ambiguous { ref candidates, score_gap } => {
            panic!(
                "expected Single but got Ambiguous with {} candidates (gap={})",
                candidates.len(),
                score_gap
            );
        }
        SymbolResolution::NotFound { qname, suggestions } => {
            panic!(
                "expected Single but got NotFound for '{}'. suggestions: {suggestions:?}",
                qname
            );
        }
    }
}

/// Helper: resolve with Aggregate policy, expect an Ambiguous result.
fn resolve_ambiguous(store: &Store, input: &SymbolInput) -> SymbolResolution {
    let res =
        resolve_symbol_input(store, input, SymbolResolutionPolicy::Aggregate)
            .expect("resolution should succeed");
    match res {
        SymbolResolution::Ambiguous { .. } => res,
        other => panic!("expected Ambiguous but got {other:?}"),
    }
}

/// Helper: extract candidates from an Ambiguous result.
fn extract_candidates(res: &SymbolResolution) -> &[ScoredCandidate] {
    match res {
        SymbolResolution::Ambiguous { candidates, .. } => candidates,
        other => panic!("expected Ambiguous but got {other:?}"),
    }
}

/// Helper: extract single resolved symbol info.
fn extract_single(res: &SymbolResolution) -> (MatchMode, &[String]) {
    match res {
        SymbolResolution::Single { resolved, .. } => {
            (resolved.match_info.mode.clone(), &resolved.match_info.ignored_mismatches)
        }
        other => panic!("expected Single but got {other:?}"),
    }
}

// ── Closed-loop round-trip tests ──────────────────────────────────────

/// search → calls round-trip:
/// 1. Call resolve with ambiguous qname → get candidates with symbol_ref
/// 2. Pass first candidate's symbol_ref back to resolve with Aggregate policy
/// 3. Verify top candidate matches the original
#[test]
fn test_search_to_calls_roundtrip() {
    let store = test_store();

    // Set up: "turn" exists in two files (creates ambiguity)
    let f1 = register_file(&store, "src/main.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/utils.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Function, "turn", "turn", 10),
            make_symbol(f2, Language::TypeScript, SymbolKind::Method, "turn", "turn", 42),
        ],
    );

    // Step 1: resolve with plain string qname → ambiguous
    let input = SymbolInput::Name("turn".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);
    assert!(candidates.len() >= 2, "should have at least 2 candidates");

    // Step 2: pick first candidate's symbol_ref and resolve with Aggregate
    let symbol_ref = SymbolInput::Selector(candidates[0].symbol_ref.clone());
    let res2 = resolve_ambiguous(&store, &symbol_ref);
    let candidates2 = extract_candidates(&res2);
    // Aggregate policy returns all qname matches as candidates.
    // The top candidate (highest score) should match the original symbol_ref.
    let top = &candidates2[0];
    assert_eq!(
        top.qualified_name,
        candidates[0].qualified_name,
        "top candidate should match original qname"
    );
    assert!(
        top.score >= candidates2.last().unwrap().score,
        "top candidate should have highest or equal score"
    );
}

/// symbol → impact round-trip:
/// 1. Resolve with ambiguous qname → get candidates
/// 2. Pass first candidate's symbol_ref to resolve with Aggregate policy
/// 3. Verify top candidate matches the original
#[test]
fn test_symbol_to_impact_roundtrip() {
    let store = test_store();

    let f1 = register_file(&store, "src/a.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/b.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Class, "Handler", "Handler", 5),
            make_symbol(f2, Language::TypeScript, SymbolKind::Class, "Handler", "Handler", 100),
        ],
    );

    // Resolve ambiguous → get symbol_ref
    let input = SymbolInput::Name("Handler".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);

    let symbol_ref = SymbolInput::Selector(candidates[0].symbol_ref.clone());
    let res2 = resolve_ambiguous(&store, &symbol_ref);
    let candidates2 = extract_candidates(&res2);
    let top = &candidates2[0];
    assert_eq!(top.qualified_name, candidates[0].qualified_name);
    assert!(top.score >= candidates2.last().unwrap().score);
}

/// symbol → explore round-trip (UniqueOrCandidates policy).
#[test]
fn test_symbol_to_explore_roundtrip() {
    let store = test_store();

    let f1 = register_file(&store, "src/x.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/y.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Function, "init", "init", 1),
            make_symbol(f2, Language::TypeScript, SymbolKind::Function, "init", "init", 99),
        ],
    );

    let input = SymbolInput::Name("init".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);

    let symbol_ref = SymbolInput::Selector(candidates[0].symbol_ref.clone());
    let res2 = resolve_single(&store, &symbol_ref);
    let (mode, mismatches) = extract_single(&res2);
    assert!(!matches!(mode, MatchMode::BestEffort), "should not be BestEffort");
    assert!(mismatches.is_empty());
}

/// symbol → context round-trip (UniqueOrCandidates policy).
#[test]
fn test_symbol_to_context_roundtrip() {
    let store = test_store();

    let f1 = register_file(&store, "src/p.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/q.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Method, "run", "run", 20),
            make_symbol(f2, Language::TypeScript, SymbolKind::Function, "run", "run", 300),
        ],
    );

    let input = SymbolInput::Name("run".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);

    let symbol_ref = SymbolInput::Selector(candidates[0].symbol_ref.clone());
    let res2 = resolve_single(&store, &symbol_ref);
    assert!(matches!(res2, SymbolResolution::Single { .. }));
}

/// explore → calls round-trip:
/// Ambiguous explore → pick candidate symbol_ref → feed to Aggregate → verify top match
#[test]
fn test_explore_to_calls_roundtrip() {
    let store = test_store();

    let f1 = register_file(&store, "src/module_a.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/module_b.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Function, "process", "process", 5),
            make_symbol(f2, Language::TypeScript, SymbolKind::Method, "process", "process", 50),
        ],
    );

    let input = SymbolInput::Name("process".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);

    let symbol_ref = SymbolInput::Selector(candidates[1].symbol_ref.clone());
    let res2 = resolve_ambiguous(&store, &symbol_ref);
    let candidates2 = extract_candidates(&res2);
    let top = &candidates2[0];
    assert_eq!(top.line, candidates[1].line);
    assert!(top.score >= candidates2.last().unwrap().score);
}

// ── Aggregation tests ─────────────────────────────────────────────────

/// calls aggregation: resolve with Aggregate policy when qname matches
/// multiple symbols → returns multiple candidates.
#[test]
fn test_calls_aggregation() {
    let store = test_store();

    let f1 = register_file(&store, "src/one.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/two.ts", Language::TypeScript);
    let f3 = register_file(&store, "src/three.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Function, "turn", "turn", 10),
            make_symbol(f2, Language::TypeScript, SymbolKind::Method, "turn", "turn", 50),
            make_symbol(f3, Language::TypeScript, SymbolKind::Function, "turn", "turn", 200),
        ],
    );

    let input = SymbolInput::Name("turn".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);

    assert!(
        candidates.len() >= 3,
        "Aggregate should return all matching candidates, got {}",
        candidates.len()
    );

    for c in candidates {
        assert_eq!(c.qualified_name, "turn", "all candidates must match the qname");
    }
}

/// impact aggregation: Aggregate policy returns all qname matches.
#[test]
fn test_impact_aggregation() {
    let store = test_store();

    let f1 = register_file(&store, "src/init_a.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/init_b.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Function, "init", "init", 1),
            make_symbol(f2, Language::TypeScript, SymbolKind::Function, "init", "init", 5),
        ],
    );

    let input = SymbolInput::Name("init".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);
    assert!(candidates.len() >= 2);

    for c in candidates {
        assert!(
            c.qualified_name.contains("init"),
            "all candidates should contain 'init' in qname, got {}",
            c.qualified_name
        );
    }
}

/// path dual aggregation: resolve both 'from' and 'to' with Aggregate policy.
#[test]
fn test_path_dual_aggregation() {
    let store = test_store();

    let f1 = register_file(&store, "src/from_a.ts", Language::TypeScript);
    let f2 = register_file(&store, "src/from_b.ts", Language::TypeScript);
    let f3 = register_file(&store, "src/to_a.ts", Language::TypeScript);
    let f4 = register_file(&store, "src/to_b.ts", Language::TypeScript);

    insert_symbols(
        &store,
        &[
            make_symbol(f1, Language::TypeScript, SymbolKind::Function, "send", "send", 10),
            make_symbol(f2, Language::TypeScript, SymbolKind::Method, "send", "send", 99),
            make_symbol(f3, Language::TypeScript, SymbolKind::Function, "recv", "recv", 5),
            make_symbol(f4, Language::TypeScript, SymbolKind::Function, "recv", "recv", 500),
        ],
    );

    let from_input = SymbolInput::Name("send".into());
    let from_res = resolve_ambiguous(&store, &from_input);
    let from_cands = extract_candidates(&from_res);
    assert!(from_cands.len() >= 2, "from should have multiple candidates");

    let to_input = SymbolInput::Name("recv".into());
    let to_res = resolve_ambiguous(&store, &to_input);
    let to_cands = extract_candidates(&to_res);
    assert!(to_cands.len() >= 2, "to should have multiple candidates");
}

// ── Fault-tolerance tests ─────────────────────────────────────────────

/// Wrong file_path doesn't block resolution for a uniquely-named symbol.
#[test]
fn test_wrong_file_path_does_not_block() {
    let store = test_store();

    let f = register_file(&store, "src/main.ts", Language::TypeScript);
    insert_symbols(
        &store,
        &[make_symbol(f, Language::TypeScript, SymbolKind::Function, "turn", "turn", 42)],
    );

    let sel = SymbolSelector {
        qualified_name: "turn".into(),
        file_path: Some("nonexistent/path.ts".into()),
        line: None,
        kind: None,
        language: None,
    };
    let input = SymbolInput::Selector(sel);
    let res = resolve_single(&store, &input);
    let (_mode, mismatches) = extract_single(&res);

    assert!(
        mismatches.contains(&"file_path".to_string()),
        "expected ignored_mismatches to contain 'file_path', got {mismatches:?}"
    );
}

/// Wrong line doesn't block resolution.
#[test]
fn test_wrong_line_does_not_block() {
    let store = test_store();

    let f = register_file(&store, "src/unique.rs", Language::Rust);
    insert_symbols(
        &store,
        &[make_symbol(f, Language::Rust, SymbolKind::Function, "unique_func", "unique_func", 10)],
    );

    let sel = SymbolSelector {
        qualified_name: "unique_func".into(),
        file_path: None,
        line: Some(99999),
        kind: None,
        language: None,
    };
    let input = SymbolInput::Selector(sel);
    let res = resolve_single(&store, &input);
    let (_mode, mismatches) = extract_single(&res);

    assert!(
        mismatches.contains(&"line".to_string()),
        "expected ignored_mismatches to contain 'line', got {mismatches:?}"
    );
}

/// Wrong kind doesn't block resolution.
#[test]
fn test_wrong_kind_does_not_block() {
    let store = test_store();

    let f = register_file(&store, "src/lib.rs", Language::Rust);
    insert_symbols(
        &store,
        &[make_symbol(f, Language::Rust, SymbolKind::Function, "unique_func", "unique_func", 1)],
    );

    let sel = SymbolSelector {
        qualified_name: "unique_func".into(),
        file_path: None,
        line: None,
        kind: Some("class".into()),
        language: None,
    };
    let input = SymbolInput::Selector(sel);
    let res = resolve_single(&store, &input);
    let (_mode, mismatches) = extract_single(&res);

    assert!(
        mismatches.contains(&"kind".to_string()),
        "expected ignored_mismatches to contain 'kind', got {mismatches:?}"
    );
}

// ── BestEffortSingle low-confidence tests ─────────────────────────────

/// BestEffortSingle policy: when two candidates have a score gap <
/// MIN_SCORE_GAP_FOR_UNIQUE (400), the mode is BestEffort instead of
/// Scored.  The higher-scored symbol is still selected.
#[test]
fn test_best_effort_single_low_confidence() {
    let store = test_store();

    // Two symbols with the same qualified_name in the same file.
    // Different kinds ensure distinct SymbolIds.
    let f = register_file(&store, "src/shared.rs", Language::Rust);
    insert_symbols(
        &store,
        &[
            make_symbol(f, Language::Rust, SymbolKind::Function, "foobar", "foobar", 12),
            make_symbol(f, Language::Rust, SymbolKind::Method, "foobar", "foobar", 13),
        ],
    );

    // Selector: line=10 → sym1 delta=2 (strong=800), sym2 delta=3 (near=500)
    // Both get the same path score (3000 – exact match).
    // Score gap = 800 - 500 = 300 < MIN_SCORE_GAP_FOR_UNIQUE (400)
    let sel = SymbolSelector {
        qualified_name: "foobar".into(),
        file_path: Some("src/shared.rs".into()),
        line: Some(10),
        kind: None,
        language: None,
    };
    let input = SymbolInput::Selector(sel);
    let res = resolve_symbol_input(&store, &input, SymbolResolutionPolicy::BestEffortSingle)
        .expect("resolution should succeed");

    match res {
        SymbolResolution::Single { resolved, .. } => {
            assert!(
                matches!(resolved.match_info.mode, MatchMode::BestEffort),
                "expected BestEffort when score gap < 400, got {:?}",
                resolved.match_info.mode
            );
            // The higher-scored symbol (line 12, delta=2 → 800)
            // should be selected over (line 13, delta=3 → 500)
            assert_eq!(
                resolved.line, 12,
                "should select the higher-scored symbol (line 12), got line {}",
                resolved.line
            );
            assert_eq!(
                resolved.file_path, "src/shared.rs",
                "file_path should match"
            );
        }
        other => panic!("expected Single but got {other:?}"),
    }
}

// ── Hex abolition tests ───────────────────────────────────────────────

/// Verify no 64-char hex string in serialized ScoredCandidate or ResolvedSymbol.
#[test]
fn test_no_hex_id_in_candidate_output() {
    let store = test_store();

    let f = register_file(&store, "src/main.ts", Language::TypeScript);
    insert_symbols(
        &store,
        &[make_symbol(f, Language::TypeScript, SymbolKind::Function, "hex_test", "hex_test", 1)],
    );

    let f2 = register_file(&store, "src/other.ts", Language::TypeScript);
    insert_symbols(
        &store,
        &[make_symbol(f2, Language::TypeScript, SymbolKind::Method, "hex_test", "hex_test", 100)],
    );

    let input = SymbolInput::Name("hex_test".into());
    let res = resolve_ambiguous(&store, &input);
    let candidates = extract_candidates(&res);

    for candidate in candidates {
        let json_str =
            serde_json::to_string(candidate).expect("ScoredCandidate should serialize");
        let json_val: serde_json::Value =
            serde_json::from_str(&json_str).expect("should be valid JSON");
        check_no_hex_id(&json_val, "ScoredCandidate");
    }

    // Also verify ResolvedSymbol
    let single_input = SymbolInput::Selector(SymbolSelector {
        qualified_name: "hex_test".into(),
        file_path: Some("src/main.ts".into()),
        line: Some(1),
        kind: Some("function".into()),
        language: None,
    });
    let single_res = resolve_single(&store, &single_input);
    if let SymbolResolution::Single { resolved, .. } = &single_res {
        let json_str = serde_json::to_string(resolved).expect("ResolvedSymbol should serialize");
        let json_val: serde_json::Value =
            serde_json::from_str(&json_str).expect("should be valid JSON");
        check_no_hex_id(&json_val, "ResolvedSymbol");
    }
}

fn check_no_hex_id(value: &serde_json::Value, context: &str) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                if key == "id" || key == "symbol_id" {
                    if let Some(s) = val.as_str() {
                        assert!(
                            !is_hex64(s),
                            "{context}: field '{key}' contains 64-char hex string: {s}",
                        );
                    }
                }
                check_no_hex_id(val, context);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                check_no_hex_id(v, context);
            }
        }
        _ => {}
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Hex input returns NotFound gracefully.
#[test]
fn test_hex_input_rejected_gracefully() {
    let store = test_store();

    let f = register_file(&store, "src/test.ts", Language::TypeScript);
    insert_symbols(
        &store,
        &[make_symbol(f, Language::TypeScript, SymbolKind::Function, "real", "real", 1)],
    );

    let hex_name = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0";
    let input = SymbolInput::Name(hex_name.to_string());

    let res = resolve_symbol_input(&store, &input, SymbolResolutionPolicy::UniqueOrCandidates)
        .expect("should not panic on hex input");

    assert!(
        matches!(res, SymbolResolution::NotFound { .. }),
        "hex input should return NotFound, got {res:?}"
    );
}
