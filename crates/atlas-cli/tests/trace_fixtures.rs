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

use atlas_analysis::trace::{Locator, Slicer, TraceEngine};
use atlas_db::Store;
use atlas_extraction::extract_file;
use atlas_graph::GraphBuilder;
use atlas_resolution::ReferenceResolver;
use atlas_types::enums::{DataFlowKind, DataNodeKind, Language};
use atlas_types::ids::FileId;
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
        let frontend = atlas_extraction::create_frontend(lang)
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
fn find_node<'a>(nodes: &'a [atlas_types::dataflow::DataNode], name: &str) -> &'a atlas_types::dataflow::DataNode {
    nodes
        .iter()
        .filter(|n| n.name.as_deref() == Some(name))
        .last()
        .unwrap_or_else(|| panic!("data node '{}' not found", name))
}

/// Assert that a trace path contains at least one step of the given edge kind.
fn assert_has_edge_kind(path: &atlas_types::trace::TracePath, kind: DataFlowKind) {
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
    resp: &atlas_analysis::trace::TraceQueryResponse<atlas_types::trace::TracePath>,
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
    let violation = path.steps.iter().any(|step| {
        step.from_node_id == inner_total.id || step.to_node_id == inner_total.id
    });
    assert!(
        !violation,
        "shadowing violation: inner 'total' (id={:?}, line={}) must NOT be in outer trace",
        inner_total.id,
        inner_total.range.start_line
    );

    // Conversely, the outer total Local MUST appear (as the definition
    // feeding the return statement).
    let outer_in_path = path.steps.iter().any(|step| {
        step.from_node_id == outer_total.id || step.to_node_id == outer_total.id
    });
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
    let data_nodes = store.find_data_nodes_by_file(&helper_id).expect("data nodes");

    // Find the parameter data node for 'base'.
    let base_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.name.as_deref() == Some("base"))
        .collect();
    assert!(!base_nodes.is_empty(), "expected parameter 'base' data node");
    let base_node = base_nodes[0];

    // Use interprocedural bridging via SummaryEdgeProvider.
    use atlas_analysis::trace::virtual_edges::SummaryEdgeProvider;
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
        arg_step.evidence.as_ref().map(|e| e.file_path.contains("main.ts")).unwrap_or(false),
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

    use atlas_analysis::trace::virtual_edges::SummaryEdgeProvider;
    let path = Slicer::slice(store.as_ref(), &point, 20, Some(&SummaryEdgeProvider))
        .expect("slice error")
        .expect("cross-file return trace must produce path");

    assert!(!path.steps.is_empty(), "cross-file return trace must have steps");

    // ── CRITICAL: Must have ReturnToCall edge ──
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);

    // The ReturnToCall edge's evidence must reference helper.ts.
    let ret_step = path
        .steps
        .iter()
        .find(|s| s.edge_kind == DataFlowKind::ReturnToCall)
        .expect("ReturnToCall step not found");
    assert!(
        ret_step.evidence.as_ref().map(|e| e.file_path.contains("helper.ts")).unwrap_or(false),
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
fn fx5_java_capability_declares_dataflow_unsupported() {
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
    // Use trace_point instead of trace_variable — Java has no DataNodes.
    let resp = engine.trace_point(&file_id, 3, 20); // line 3 ~ column 20

    // Must NOT crash. Response must be ok=true.
    assert!(resp.ok, "Java trace_point must return ok=true");

    // Capability must be present and indicate Symbolic level (no dataflow).
    let cap = resp.capability.as_ref().expect("Java capability must be present");
    assert_eq!(cap.language, "java");
    assert!(
        cap.capability_level == atlas_types::capability::CapabilityLevel::Symbolic,
        "Java must be Symbolic level, got {:?}",
        cap.capability_level
    );

    // Feature matrix must declare dataflow features as Unsupported.
    if let Some(ref features) = cap.features {
        assert!(
            matches!(
                features.local_dataflow,
                atlas_types::capability::FeatureSupport::Unsupported { .. }
            ),
            "Java local_dataflow must be Unsupported"
        );
        assert!(
            matches!(
                features.use_def,
                atlas_types::capability::FeatureSupport::Unsupported { .. }
            ),
            "Java use_def must be Unsupported"
        );
    }

    // These features must NOT be in the supported_features flat list.
    for unsupported in &["local_dataflow", "use_def", "field_access"] {
        assert!(
            !cap.supported_features.contains(&(*unsupported).to_string()),
            "Java must not claim '{}' in supported_features",
            unsupported
        );
    }
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
    let cap = resp.capability.as_ref().expect("Python capability must exist");
    assert_eq!(cap.language, "python");

    // Check feature matrix for CFG support.
    if let Some(ref features) = cap.features {
        assert!(
            matches!(
                features.cfg,
                atlas_types::capability::FeatureSupport::Unsupported { .. }
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

    // The path should include a FieldLoad edge.
    let has_field_load = path
        .steps
        .iter()
        .any(|s| s.edge_kind == DataFlowKind::FieldLoad);
    assert!(has_field_load, "should have at least one FieldLoad edge");
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
        ("a.ts", r#"import { b } from './b';
export function a(): void { b(42); }
"#),
        ("b.ts", r#"import { c } from './c';
export function b(x: number): void { c(x); }
"#),
        ("c.ts", r#"export function c(val: number): void {
    console.log(val);
}
"#),
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
    let arg_to_param_count = path.steps.iter()
        .filter(|s| s.edge_kind == DataFlowKind::ArgToParam)
        .count();
    assert!(
        arg_to_param_count >= 1,
        "expected >= 1 ArgToParam edge in indirect chain, got {}",
        arg_to_param_count
    );

    // Should have evidence from file a.ts (the indirect caller)
    let has_a_evidence = path.steps.iter().any(|s| {
        s.evidence.as_ref().map_or(false, |e| e.file_path.contains("a.ts"))
    });
    assert!(has_a_evidence, "should have evidence from indirect caller a.ts");
}


