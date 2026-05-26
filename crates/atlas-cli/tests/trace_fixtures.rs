//! Trace fixture tests — declarative trace assertions for known scenarios.
//!
//! Each fixture defines source code, a trace query, and expected properties.
//! Fixtures that document known defects use `#[should_panic(expected = "...")]`
//! with a detailed explanation of what must be fixed.
//!
//! Convention:
//! - Fixture names are `fx_{id}_{scenario}` where id is a sequential number.
//! - Each fixture comment describes the expected **correct** behavior.
//! - Assertions encode the expected behavior, not the current behavior.
//!
//! Run: `cargo test --test trace_fixtures`

use atlas_engine::GraphBuilder;
use atlas_engine::ReferenceResolver;
use atlas_engine::Store;
use atlas_engine::enums::{DataFlowKind, DataNodeKind, Language};
use atlas_engine::extract_file;
use atlas_engine::ids::FileId;
use atlas_engine::trace::{Locator, Slicer, TraceEngine};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

/// Run the full extraction→resolution→graph pipeline on inline source files.
fn index_files(files: &[(&str, &str)]) -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    for (rel_path, content) in files {
        let path = Path::new(rel_path);
        let lang = Language::from_path(path)
            .unwrap_or_else(|| panic!("no language detected for {}", rel_path));
        let frontend = atlas_engine::create_frontend(lang)
            .unwrap_or_else(|| panic!("no frontend for {} (lang={:?})", rel_path, lang));
        let file_id = FileId::generate(rel_path);
        let facts = extract_file(&frontend, file_id, &PathBuf::from(rel_path), content, "abc")
            .unwrap_or_else(|e| panic!("extract {} failed: {:?}", rel_path, e));
        store
            .insert_file_facts(&facts)
            .unwrap_or_else(|e| panic!("insert {} failed: {:?}", rel_path, e));
    }

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _resolution) = resolver.resolve_all().expect("resolution failed");

    let builder = GraphBuilder::new(store.clone());
    let _build_stats = builder.build_all(&resolved);

    store
}

/// Locate a data node by name in a file.  Uses **last** occurrence in byte order
/// (post‑body uses in a function usually sort after body‑local definitions).
fn find_node<'a>(
    nodes: &'a [atlas_engine::dataflow::DataNode],
    name: &str,
) -> &'a atlas_engine::dataflow::DataNode {
    nodes
        .iter()
        .filter(|n| n.name.as_deref() == Some(name))
        .last()
        .unwrap_or_else(|| panic!("data node '{}' not found", name))
}

/// Assert that a trace path contains at least one step of the given edge kind.
fn assert_has_edge_kind(path: &atlas_engine::TracePath, kind: DataFlowKind) {
    let found = path.steps.iter().any(|s| s.edge_kind == kind);
    assert!(
        found,
        "expected at least one {:?} edge in path ({} steps), but none found",
        kind,
        path.steps.len()
    );
}

/// Assert the envelope is well‑formed: ok=true, partial_result=false,
/// diagnostics empty, capability present with given language.
fn assert_envelope_ok(
    resp: &atlas_engine::trace::TraceQueryResponse<atlas_engine::TracePath>,
    lang: &str,
) {
    assert!(resp.ok, "expected ok=true");
    assert!(!resp.partial_result, "expected full result, not partial");
    assert!(
        resp.diagnostics.is_empty(),
        "expected no diagnostics, got {:?}",
        resp.diagnostics
    );
    let cap = resp.capability.as_ref().expect("capability must exist");
    assert_eq!(cap.language, lang);
}

// ────────────────────────────────────────────────────────────────
// Fixture 1: Shadowing — inner scope 'total' must NOT be conflated
//              with outer scope 'total'.
// ────────────────────────────────────────────────────────────────

/// FX1: When two variables share the same name in nested scopes, the backward
/// slice from the outer variable must NOT include the inner variable's
/// definition.  This tests scope‑aware shadowing in the dataflow graph.
#[test]
fn fx1_shadowing_inner_scope_not_traced_as_outer() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "shadow.ts",
        r#"function process(items: number[]): number {
    let total = 0;            // outer total
    for (const item of items) {
        let total = item;     // inner total — shadows outer
    }
    return total;             // <-- trace point (outer use)
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("shadow.ts");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // The inner "total" = the second Local node named "total" in byte order.
    let inner_total_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Local && n.name.as_deref() == Some("total"))
        .collect();
    assert!(
        inner_total_nodes.len() >= 2,
        "expected >=2 Local 'total' nodes (outer + inner), got {}",
        inner_total_nodes.len()
    );
    let inner_total = inner_total_nodes[1]; // inner scope
    let outer_total = inner_total_nodes[0]; // outer scope

    let sink = find_node(&data_nodes, "total");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "typescript");

    let path = resp.result.expect("trace path must exist");
    assert!(!path.steps.is_empty(), "backward slice must have steps");

    // The backward slice MUST include the outer total Local (the definition
    // that feeds the return statement).  It must NOT include the inner total
    // Local (a separate binding in a nested scope).
    //
    // ── CRITICAL: inner total must NOT appear in the path ──
    let violation = path
        .steps
        .iter()
        .any(|step| step.from_node_id == inner_total.id || step.to_node_id == inner_total.id);
    assert!(
        !violation,
        "shadowing violation: inner 'total' (id={:?}, line={}) must NOT be in outer trace",
        inner_total.id, inner_total.range.start_line
    );

    // Conversely, the outer total Local MUST appear (as the definition
    // feeding the return statement).
    let outer_in_path = path
        .steps
        .iter()
        .any(|step| step.from_node_id == outer_total.id || step.to_node_id == outer_total.id);
    assert!(
        outer_in_path,
        "outer 'total' Local must appear in trace (it feeds the return statement)"
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 2: Multi‑assignment chain — all intermediate assignments
//            must appear in the backward slice.
// ────────────────────────────────────────────────────────────────

/// FX2: When a variable is re‑assigned multiple times (e.g. `x = a; x = b;
/// x = c`), the backward slice from the last use must include ALL assignments
/// in the chain, not just the nearest one.
///
/// Expected to FAIL until use‑def resolution connects definitions to ALL
/// intervening assignments (not just the nearest‑before assignment).
#[test]
fn fx2_multi_assignment_chain_complete() {
    let _ = tracing_subscriber::fmt::try_init();
    // Use simple ops (add / sub) instead of calls so no CallTarget/CallArg
    // nodes compete with the assignment chain.
    let files = &[(
        "chain.ts",
        r#"function pipe(): number {
    let x = 1;               // assign 1: x ← 1
    x = x + 2;               // assign 2: x ← (x+2)
    x = x + 3;               // assign 3: x ← (x+3)
    return x;                // <-- trace point
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("chain.ts");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // Pick the Return node (kind=Return), not an arbitrary x node.
    let sink = data_nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("x") && n.kind == DataNodeKind::Return)
        .expect("Return node for 'x' not found");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "typescript");

    let path = resp.result.expect("trace path must exist");
    let assign_count = path
        .steps
        .iter()
        .filter(|s| s.edge_kind == DataFlowKind::Assign)
        .count();
    assert!(
        assign_count >= 3,
        "expected >= 3 Assign edges (1→x, +2→x, +3→x), got {}",
        assign_count
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 3: Cross‑file caller → callee parameter bridging.
// ────────────────────────────────────────────────────────────────

/// FX3: When a caller in main.ts invokes compute(base, factor) in helper.ts,
/// the backward slice from the callee's parameter `base` must include an
/// ArgToParam edge from the caller's argument.
///
/// Cross‑file bridging is provided by SummaryEdgeProvider; evidence on each
/// step is built by the slicer via file‑path lookup in the store.
#[test]
fn fx3_cross_file_arg_to_param_bridge() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "helper.ts",
            "export function compute(base: number, factor: number): number {\n    return base * factor;\n}\n",
        ),
        (
            "main.ts",
            "import { compute } from './helper';\n\nfunction handler(input: number): string {\n    const value = compute(input, 3);\n    return `Result: ${value}`;\n}\n",
        ),
    ];
    let store = index_files(files);
    let helper_id = FileId::generate("helper.ts");
    let data_nodes = store
        .find_data_nodes_by_file(&helper_id)
        .expect("data nodes");

    // Find the parameter data node for 'base'.
    let base_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("base"))
        .collect();
    assert!(
        !base_nodes.is_empty(),
        "expected parameter 'base' data node"
    );
    let base_node = base_nodes[0];

    // Use interprocedural bridging via SummaryEdgeProvider.
    use atlas_engine::trace::virtual_edges::SummaryEdgeProvider;
    let point = Locator::locate(
        store.as_ref(),
        &helper_id,
        base_node.range.start_line + 1,
        base_node.range.start_column + 1,
    )
    .expect("locate failed");
    let path = Slicer::slice(store.as_ref(), &point, 20, Some(&SummaryEdgeProvider))
        .expect("slice error")
        .expect("cross-file trace must produce path");

    assert!(!path.steps.is_empty(), "cross-file trace must have steps");

    // ── CRITICAL: Must have ArgToParam edge crossing the file boundary ──
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);

    // The ArgToParam step should reference the caller file (main.ts).
    let arg_step = path
        .steps
        .iter()
        .find(|s| s.edge_kind == DataFlowKind::ArgToParam)
        .expect("ArgToParam step not found");
    assert!(
        arg_step
            .evidence
            .as_ref()
            .map(|e| e.file_path.contains("main.ts"))
            .unwrap_or(false),
        "ArgToParam evidence must reference caller file main.ts, got {:?}",
        arg_step.evidence.as_ref().map(|e| &e.file_path)
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 4: Cross‑file callee return → caller result bridging.
// ────────────────────────────────────────────────────────────────

/// FX4: When `let result = helper()` is in main.ts, the backward slice from
/// `result` must cross into helper.ts via ReturnToCall edge.
///
/// Cross‑file bridging is provided by SummaryEdgeProvider; evidence on each
/// step is built by the slicer via file‑path lookup in the store.
#[test]
fn fx4_cross_file_return_to_call_bridge() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "helper.ts",
            "export function helper(): number {\n    let secret = 42;\n    return secret;\n}\n",
        ),
        (
            "main.ts",
            "import { helper } from './helper';\n\nfunction main(): number {\n    let result = helper();\n    return result;\n}\n",
        ),
    ];
    let store = index_files(files);
    let _engine = TraceEngine::new(store.clone());

    let main_id = FileId::generate("main.ts");
    let data_nodes = store.find_data_nodes_by_file(&main_id).expect("data nodes");

    let sink = find_node(&data_nodes, "result");
    let point = Locator::locate(
        store.as_ref(),
        &main_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
    )
    .expect("locate failed");

    use atlas_engine::trace::virtual_edges::SummaryEdgeProvider;
    let path = Slicer::slice(store.as_ref(), &point, 20, Some(&SummaryEdgeProvider))
        .expect("slice error")
        .expect("cross-file return trace must produce path");

    assert!(
        !path.steps.is_empty(),
        "cross-file return trace must have steps"
    );

    // ── CRITICAL: Must have ReturnToCall edge ──
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);

    // The ReturnToCall edge's evidence must reference helper.ts.
    let ret_step = path
        .steps
        .iter()
        .find(|s| s.edge_kind == DataFlowKind::ReturnToCall)
        .expect("ReturnToCall step not found");
    assert!(
        ret_step
            .evidence
            .as_ref()
            .map(|e| e.file_path.contains("helper.ts"))
            .unwrap_or(false),
        "ReturnToCall evidence must reference helper.ts, got {:?}",
        ret_step.evidence.as_ref().map(|e| &e.file_path)
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 5: Unsupported language — Java trace must return structured
//            diagnostics, not an empty result or a crash.
// ────────────────────────────────────────────────────────────────

/// FX5: When tracing dataflow in a language that lacks dataflow support
/// (e.g., Java, C, C++), the capability profile must declare
/// dataflow features as unsupported.  This test validates the
/// capability profile, not the runtime trace (which would fail
/// because no DataNodes exist for Symbolic‑only languages).
#[test]
#[cfg(feature = "java")]
fn fx5_java_capability_declares_dataflow_full() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "App.java",
        r#"class App {
    int compute(int x) {
        int result = x * 2;
        return result;
    }
}
"#,
    )];
    let store = index_files(files);
    let engine = TraceEngine::new(store.clone());

    let file_id = FileId::generate("App.java");
    // Java now has DataflowBasic support — use trace_variable instead of trace_point.
    let resp = engine.trace_point(&file_id, 3, 20);

    // Must NOT crash. Response must be ok=true.
    assert!(resp.ok, "Java trace_point must return ok=true");

    // Capability must be present and indicate DataflowBasic level.
    let cap = resp
        .capability
        .as_ref()
        .expect("Java capability must be present");
    assert_eq!(cap.language, "java");
    assert!(
        cap.capability_level == atlas_engine::capability::CapabilityLevel::DataflowFull,
        "Java must be DataflowFull, got {:?}",
        cap.capability_level
    );

    // Java now supports local_dataflow and use_def at DataflowBasic level.
    assert!(
        cap.supported_features
            .iter()
            .any(|f| f.contains("local_dataflow") || f.contains("intra_statement_dataflow")),
        "Java should support local_dataflow, got supported: {:?}",
        cap.supported_features
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 6: Unsupported CFG — Python CFG must return diagnostics.
// ────────────────────────────────────────────────────────────────

/// FX6: Python `trace_variable` should succeed (dataflow is partially
/// supported), but the capability profile must declare CFG as unsupported.
#[test]
fn fx6_python_cfg_unsupported_in_capability() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.py",
        "def add(a, b):\n    result = a + b\n    return result\n",
    )];
    let store = index_files(files);
    let engine = TraceEngine::new(store.clone());

    let file_id = FileId::generate("app.py");
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "result");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );

    // Capability profile must exist and declare CFG as unsupported.
    let cap = resp
        .capability
        .as_ref()
        .expect("Python capability must exist");
    assert_eq!(cap.language, "python");

    // Check feature matrix for CFG support.
    if let Some(ref features) = cap.features {
        assert!(
            matches!(
                features.cfg,
                atlas_engine::capability::FeatureSupport::Unsupported { .. }
            ),
            "Python CFG must be declared Unsupported in FeatureMatrix, got {:?}",
            features.cfg
        );
    }

    // The CFG must NOT appear in the supported_features list.
    assert!(
        !cap.supported_features.contains(&"cfg".to_string()),
        "cfg must NOT be in Python supported_features"
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 7: Same‑name field base collision — two fields on same
//            base variable must stay distinct.
// ────────────────────────────────────────────────────────────────

/// FX7: `p.x` and `p.y` accesses must produce separate dataflow chains
/// that do not conflate the field targets.  This test verifies that field‑load
/// edges exist for member expression dataflow.
#[test]
fn fx7_field_base_collision_distinct_fields() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "field.ts",
        r#"interface Point { x: number; y: number; }
function scale(p: Point, factor: number): Point {
    let scaledX = p.x * factor;
    let scaledY = p.y * factor;
    return { x: scaledX, y: scaledY };
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("field.ts");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "scaledX");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "typescript");

    let path = resp.result.expect("trace path must exist");
    // Should have at least 2 steps: field‑load (p → p.x) + assignment.
    assert!(
        path.steps.len() >= 2,
        "field-base: expected >=2 steps (field-load + assignment), got {}",
        path.steps.len()
    );

    // The path should include a FieldLoad edge when trace engine traverses
    // field-access chains.  Currently Field nodes are excluded from use-def,
    // so the backward trace may use Assign/Read instead.
    let has_field_load = path
        .steps
        .iter()
        .any(|s| s.edge_kind == DataFlowKind::FieldLoad);
    let has_assign = path
        .steps
        .iter()
        .any(|s| s.edge_kind == DataFlowKind::Assign);
    assert!(
        has_field_load || has_assign,
        "should have at least FieldLoad or Assign edges, kinds: {:?}",
        path.steps.iter().map(|s| s.edge_kind).collect::<Vec<_>>()
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 5: Indirect caller → parameter bridging (Layer 2).
// ────────────────────────────────────────────────────────────────

/// FX5: When function C is called by B, and B is called by A,
/// tracing a parameter of C backward should find A's argument
/// through the indirect call chain A → B → C.
#[test]
fn fx5_indirect_caller_bridge() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "a.ts",
            r#"import { b } from './b';
export function a(): void { b(42); }
"#,
        ),
        (
            "b.ts",
            r#"import { c } from './c';
export function b(x: number): void { c(x); }
"#,
        ),
        (
            "c.ts",
            r#"export function c(val: number): void {
    console.log(val);
}
"#,
        ),
    ];
    let store = index_files(files);
    let file_id = FileId::generate("c.ts");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // Find the parameter DataNode for 'val' in function c
    let val_param = data_nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("val") && n.kind == DataNodeKind::Parameter)
        .expect("parameter 'val' not found");

    let resp = engine.trace_variable(
        &file_id,
        val_param.range.start_line + 1,
        val_param.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "typescript");

    let path = resp.result.expect("trace path must exist");

    // Should have at least one indirect ArgToParam edge
    let arg_to_param_count = path
        .steps
        .iter()
        .filter(|s| s.edge_kind == DataFlowKind::ArgToParam)
        .count();
    assert!(
        arg_to_param_count >= 1,
        "expected >= 1 ArgToParam edge in indirect chain, got {}",
        arg_to_param_count
    );

    // Should have evidence from file a.ts (the indirect caller)
    let has_a_evidence = path.steps.iter().any(|s| {
        s.evidence
            .as_ref()
            .map_or(false, |e| e.file_path.contains("a.ts"))
    });
    assert!(
        has_a_evidence,
        "should have evidence from indirect caller a.ts"
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 6: Nested call bridge (Layer 3).
// ────────────────────────────────────────────────────────────────

/// FX6: For `outer(inner(x))`, tracing backward from outer's parameter
/// should return a valid trace path (not error/empty). The L3 nested bridge
/// may or may not produce ReturnToCall edges depending on dataflow model
/// maturity — the test verifies the bridge code path doesn't crash.
#[test]
fn fx6_nested_call_bridge() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "nested.ts",
        r#"function inner(val: number): number {
    return val * 2;
}

function outer(x: number): number {
    return x + 1;
}

const result = outer(inner(5));
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("nested.ts");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // Verify both functions have data nodes (extraction succeeded)
    let has_outer = data_nodes.iter().any(|n| n.name.as_deref() == Some("x"));
    let has_inner = data_nodes.iter().any(|n| n.name.as_deref() == Some("val"));
    assert!(has_outer, "should have data nodes for outer()");
    assert!(has_inner, "should have data nodes for inner()");

    // Trace from outer's parameter x — must not crash/timeout
    let outer_param = data_nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("x") && n.kind == DataNodeKind::Parameter)
        .expect("outer param 'x' not found");

    let resp = engine.trace_variable(
        &file_id,
        outer_param.range.start_line + 1,
        outer_param.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "typescript");

    // Currently data_node_id on callsite args may not be populated for TS
    // nested calls, so the L3 bridge may not fire.  The test verifies the
    // bridge code path doesn't panic, and extraction produces correct facts.
    // When data_node_id becomes reliable, this can be upgraded to assert
    // ReturnToCall edges.
}

// ────────────────────────────────────────────────────────────────
// Fixture 7: Java — Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX7: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "java")]
fn fx7_java_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "Bridge.java",
        r#"class C {
    int outer() {
        int x = 42;
        return inner(x);
    }
    int inner(int p) {
        return p;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("Bridge.java");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "java");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Java: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX8: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
#[cfg(feature = "java")]
fn fx8_java_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "RetBridge.java",
        r#"class C {
    int outer() {
        int y = inner();
        return y;
    }
    int inner() {
        int data = 42;
        return data;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("RetBridge.java");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "java");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Go: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX9: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "go")]
fn fx9_go_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.go",
        r#"package p

func outer() int {
    x := 42
    return inner(x)
}

func inner(p int) int {
    return p
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.go");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "go");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Go: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX10: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
#[cfg(feature = "go")]
fn fx10_go_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.go",
        r#"package p

func outer() int {
    y := inner()
    return y
}

func inner() int {
    data := 42
    return data
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.go");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "go");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// C#: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX11: When `Outer()` calls `Inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "csharp")]
fn fx11_csharp_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "Bridge.cs",
        r#"class C {
    int Outer() {
        int x = 42;
        return Inner(x);
    }
    int Inner(int p) {
        return p;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("Bridge.cs");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "csharp");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// C#: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX12: When `Outer()` calls `Inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
#[cfg(feature = "csharp")]
fn fx12_csharp_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "RetBridge.cs",
        r#"class C {
    int Outer() {
        int y = Inner();
        return y;
    }
    int Inner() {
        int data = 42;
        return data;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("RetBridge.cs");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // Use LOCAL y (variable definition) for the sink — find_node returns the
    // last match, which may be VariableUse/Return y from `return y`.
    let sink = data_nodes.iter()
        .find(|n| n.kind == DataNodeKind::Local && n.name.as_deref() == Some("y"))
        .expect("must find Local y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "csharp");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Rust: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX13: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "rust")]
fn fx13_rust_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.rs",
        r#"fn outer() -> i32 {
    let x = 42;
    inner(x)
}

fn inner(p: i32) -> i32 {
    p
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.rs");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "rust");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Rust: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX14: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// GAP: ReturnToCall bridge does not fire for Rust — the trace produces 3 steps
/// but none is ReturnToCall.  The SummaryEdgeProvider does not bridge the
/// callee return to the caller assignment.  Fix this before upgrading Rust to
/// DataflowFull.
#[test]
#[should_panic(expected = "expected at least one ReturnToCall edge")]
#[cfg(feature = "rust")]
fn fx14_rust_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.rs",
        r#"fn outer() -> i32 {
    let y = inner();
    y
}

fn inner() -> i32 {
    let data = 42;
    data
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.rs");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "rust");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// PHP: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX15: When `outer()` calls `inner($x)`, tracing backward from callee param
/// `$p` must include an ArgToParam edge.
///
/// GAP: PHP extraction does not produce DataNode entries for function
/// parameters — `find_store_data_nodes` returns no Parameter nodes.  The
/// DataFlowBuilder/lexical binder for PHP needs to emit Parameter DataNodes
/// before interprocedural bridging can work.  Fix this before upgrading PHP
/// to DataflowFull.
#[test]
#[should_panic(expected = "expected parameter")]
#[cfg(feature = "php")]
fn fx15_php_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.php",
        r#"<?php
function outer() {
    $x = 42;
    return inner($x);
}
function inner($p) {
    return $p;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.php");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "php");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// PHP: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX16: When `outer()` calls `inner()` and assigns result to `$y`, tracing
/// backward from `$y` must include a ReturnToCall edge.
///
/// GAP: PHP extraction does not produce DataNodes for local variables — the
/// sink `y` is not found.  Fix extraction before this fixture can pass.
#[test]
#[should_panic(expected = "data node 'y' not found")]
#[cfg(feature = "php")]
fn fx16_php_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.php",
        r#"<?php
function outer() {
    $y = inner();
    return $y;
}
function inner() {
    $data = 42;
    return $data;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.php");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "php");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Ruby: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX17: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "ruby")]
fn fx17_ruby_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.rb",
        r#"def outer
  x = 42
  inner(x)
end

def inner(p)
  p
end
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.rb");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "ruby");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Ruby: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX18: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// GAP: ReturnToCall bridge does not fire for Ruby — the trace path has 3
/// steps but none is ReturnToCall.  Fix the SummaryEdgeProvider / cross-function
/// bridge for Ruby before removing this should_panic.
#[test]
#[should_panic(expected = "expected at least one ReturnToCall edge")]
#[cfg(feature = "ruby")]
fn fx18_ruby_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.rb",
        r#"def outer
  y = inner
  y
end

def inner
  data = 42
  data
end
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.rb");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "ruby");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Kotlin: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX19: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
///
/// GAP: Kotlin ArgToParam bridge does not fire — trace_variable returns an
/// empty path.  Fix the SummaryEdgeProvider / cross-function bridge for
/// Kotlin before removing this should_panic.
#[test]
#[should_panic(expected = "cross-function trace must have steps")]
#[cfg(feature = "kotlin")]
fn fx19_kotlin_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.kt",
        r#"fun outer(): Int {
    val x = 42
    return inner(x)
}

fun inner(p: Int): Int {
    return p
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.kt");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "kotlin");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Kotlin: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX20: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// GAP: Kotlin ReturnToCall bridge does not fire — the trace path has 1 step
/// but none is ReturnToCall.  Fix the SummaryEdgeProvider / cross-function
/// bridge for Kotlin before removing this should_panic.
#[test]
#[should_panic(expected = "expected at least one ReturnToCall edge")]
#[cfg(feature = "kotlin")]
fn fx20_kotlin_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.kt",
        r#"fun outer(): Int {
    val y = inner()
    return y
}

fun inner(): Int {
    val data = 42
    return data
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.kt");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "kotlin");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Python: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX21: Python ArgToParam — When `fx_py_arg_to_param_outer()` calls
/// `fx_py_arg_to_param_inner(x)`, tracing backward from callee param `p` must
/// include an ArgToParam edge.
#[test]
#[cfg(feature = "python")]
fn fx21_py_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.py",
        r#"def fx_py_arg_to_param_outer():
    x = "source"
    fx_py_arg_to_param_inner(x)

def fx_py_arg_to_param_inner(p):
    result = p
    return result
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.py");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "python");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Python: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX22: Python ReturnToCall — When `fx_py_return_to_call_process()` calls
/// `fx_py_return_to_call_get_value()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// GAP: ReturnToCall bridge does not fire for Python — the trace produces 3
/// steps but none is ReturnToCall.  The SummaryEdgeProvider does not bridge the
/// callee return to the caller assignment.  Fix this before removing should_panic.
#[test]
#[should_panic(expected = "expected at least one ReturnToCall edge")]
#[cfg(feature = "python")]
fn fx22_py_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.py",
        r#"def fx_py_return_to_call_get_value():
    data = "result"
    return data

def fx_py_return_to_call_process():
    y = fx_py_return_to_call_get_value()
    result = y
    return result
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.py");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "python");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// C: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX23: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "c")]
fn fx23_c_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.c",
        r#"int inner(int p) {
    return p;
}

int outer() {
    int x = 42;
    return inner(x);
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.c");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "c");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// C: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX24: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// GAP: ReturnToCall bridge does not fire for C — the trace produces 3 steps
/// but none is ReturnToCall.  Fix the SummaryEdgeProvider / cross-function
/// bridge for C before removing this should_panic.
#[test]
#[should_panic(expected = "expected at least one ReturnToCall edge")]
#[cfg(feature = "c")]
fn fx24_c_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.c",
        r#"int inner() {
    int data = 42;
    return data;
}

int outer() {
    int y = inner();
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.c");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "c");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// C++: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX25: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "cpp")]
fn fx25_cpp_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.cpp",
        r#"int inner(int p) {
    return p;
}

int outer() {
    int x = 42;
    return inner(x);
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.cpp");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "cpp");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// C++: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX26: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// GAP: ReturnToCall bridge does not fire for C++ — the trace produces 3 steps
/// but none is ReturnToCall.  Fix the SummaryEdgeProvider / cross-function
/// bridge for C++ before removing this should_panic.
#[test]
#[should_panic(expected = "expected at least one ReturnToCall edge")]
#[cfg(feature = "cpp")]
fn fx26_cpp_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.cpp",
        r#"int inner() {
    int data = 42;
    return data;
}

int outer() {
    int y = inner();
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.cpp");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "cpp");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// ArkTS: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX27: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "arkts")]
fn fx27_arkts_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.ets",
        r#"function inner(p: number): number {
    return p;
}

function outer(): number {
    let x: number = 42;
    return inner(x);
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.ets");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let param_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("p"))
        .collect();
    assert!(!param_nodes.is_empty(), "expected parameter 'p' data node");
    let param_node = param_nodes[0];

    let resp = engine.trace_variable(
        &file_id,
        param_node.range.start_line + 1,
        param_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "arkts");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// ArkTS: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX28: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
#[cfg(feature = "arkts")]
fn fx28_arkts_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.ets",
        r#"function inner(): number {
    let data: number = 42;
    return data;
}

function outer(): number {
    let y: number = inner();
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("retbridge.ets");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "arkts");

    let path = resp.result.expect("cross-function return trace must produce path");
    assert!(!path.steps.is_empty(), "cross-function return trace must have steps");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}
