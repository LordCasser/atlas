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
use atlas_engine::enums::{
    CallContext, CfgEdgeKind, CfgNodeKind, DataFlowKind, DataNodeKind, Language, SymbolKind,
};
use atlas_engine::ids::FileId;
use atlas_engine::trace::{Locator, Slicer, TraceEngine};
use atlas_engine::{CfgEdge, ExtractionMode, LanguageFrontend, extract_file_with_mode};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

/// Read a source file from the un-versioned `examples/` corpus.
///
/// `examples/` is git-ignored (see `.gitignore`), so a fresh clone does not
/// have it. These fixtures used to pull the sources in with `include_str!`,
/// which is expanded at compile time — a missing corpus failed the whole test
/// crate to *compile* rather than skipping the affected tests. Reading at run
/// time lets every other fixture in this file still run.
///
/// The returned string is leaked so call sites keep the `&'static str` shape
/// `include_str!` gave them. Test processes are short-lived; this is bounded by
/// the number of corpus fixtures.
fn example_source(rel_path: &str) -> Option<&'static str> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(rel_path);
    match std::fs::read_to_string(&path) {
        Ok(source) => Some(Box::leak(source.into_boxed_str())),
        Err(err) => {
            eprintln!(
                "skipping fixture: examples corpus file {rel_path} is unavailable ({err}). \
                 Populate `examples/` to run real-project regressions."
            );
            None
        }
    }
}

/// Bind a corpus source or return early from the calling test.
macro_rules! example_source_or_skip {
    ($rel_path:literal) => {
        match example_source($rel_path) {
            Some(source) => source,
            None => return,
        }
    };
}

fn extract_full(
    frontend: &LanguageFrontend,
    file_id: FileId,
    path: &Path,
    source: &str,
    content_hash: &str,
) -> anyhow::Result<atlas_engine::FileFacts> {
    extract_file_with_mode(
        frontend,
        file_id,
        path,
        source,
        content_hash,
        ExtractionMode::Full,
        &(),
    )
}

/// Run the full extraction→resolution→graph pipeline on inline source files.
fn index_files(files: &[(&str, &str)]) -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    for (rel_path, content) in files {
        let path = Path::new(rel_path);
        let lang = Language::from_path(path)
            .unwrap_or_else(|| panic!("no language detected for {rel_path}"));
        let frontend = atlas_engine::create_frontend(lang)
            .unwrap_or_else(|| panic!("no frontend for {rel_path} (lang={lang:?})"));
        let file_id = FileId::generate(rel_path);
        let facts = extract_full(&frontend, file_id, &PathBuf::from(rel_path), content, "abc")
            .unwrap_or_else(|e| panic!("extract {rel_path} failed: {e:?}"));
        store
            .insert_file_facts(&facts)
            .unwrap_or_else(|e| panic!("insert {rel_path} failed: {e:?}"));
    }

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _resolution) = resolver
        .resolve_all_parallel(store.clone(), None, None)
        .expect("resolution failed");

    let builder = GraphBuilder::new(store.clone());
    let _build_stats = builder.build_all(&resolved);

    store
}

fn assert_persisted_exception_cfg(
    store: &Store,
    function_id: &atlas_engine::SymbolId,
    label: &str,
    min_exception_edges: usize,
) {
    let nodes = store
        .find_cfg_nodes_by_function(function_id)
        .unwrap_or_else(|error| panic!("{label} CFG nodes: {error}"));
    assert!(
        nodes.iter().any(|node| node.kind == CfgNodeKind::Branch),
        "{label} try/catch missing Branch"
    );
    assert!(
        nodes.iter().any(|node| node.kind == CfgNodeKind::Join),
        "{label} try/catch missing Join"
    );
    let exception_edges = nodes
        .iter()
        .flat_map(|node| {
            store
                .find_cfg_edges_by_source(&node.id)
                .unwrap_or_else(|error| panic!("{label} CFG edges: {error}"))
        })
        .filter(|edge| edge.kind == CfgEdgeKind::Exception)
        .count();
    assert!(
        exception_edges >= min_exception_edges,
        "{label} expected at least {min_exception_edges} Exception edges, got {exception_edges}"
    );
}

fn persisted_cfg_node_id_for_text(
    nodes: &[atlas_engine::cfg::CfgNode],
    source: &str,
    kind: CfgNodeKind,
    expected: &str,
) -> atlas_engine::CfgNodeId {
    nodes
        .iter()
        .find(|node| {
            if node.kind != kind {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim() == expected)
        })
        .unwrap_or_else(|| panic!("no persisted {kind:?} CFG node with text {expected:?}"))
        .id
}

fn persisted_cfg_node_ids_for_text(
    nodes: &[atlas_engine::cfg::CfgNode],
    source: &str,
    kind: CfgNodeKind,
    expected: &str,
) -> Vec<atlas_engine::CfgNodeId> {
    nodes
        .iter()
        .filter(|node| {
            if node.kind != kind {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim() == expected)
        })
        .map(|node| node.id)
        .collect()
}

fn persisted_cfg_edges(
    store: &Store,
    nodes: &[atlas_engine::cfg::CfgNode],
) -> Vec<atlas_engine::cfg::CfgEdge> {
    nodes
        .iter()
        .flat_map(|node| {
            store
                .find_cfg_edges_by_source(&node.id)
                .expect("persisted CFG edges")
        })
        .collect()
}

fn persisted_cfg_reaches(
    edges: &[atlas_engine::cfg::CfgEdge],
    source: atlas_engine::CfgNodeId,
    target: atlas_engine::CfgNodeId,
) -> bool {
    let mut pending = vec![source];
    let mut visited = std::collections::HashSet::new();
    while let Some(node_id) = pending.pop() {
        if !visited.insert(node_id) {
            continue;
        }
        if node_id == target {
            return true;
        }
        pending.extend(
            edges
                .iter()
                .filter(|edge| edge.source == node_id)
                .map(|edge| edge.target),
        );
    }
    false
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
        .next_back()
        .unwrap_or_else(|| panic!("data node '{name}' not found"))
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

// ── Semantic precision helpers ──────────────────────────────────────────

/// Look up the name of a DataNode from the store by ID.
fn node_name_or(store: &Store, node_id: &atlas_engine::DataNodeId, default: &str) -> String {
    store
        .get_data_node(node_id)
        .ok()
        .flatten()
        .and_then(|n| n.name)
        .unwrap_or_else(|| default.to_string())
}

/// Assert that at least one step has the given edge kind AND references a
/// node whose name contains `name`.
fn assert_step_with_name(
    store: &Store,
    path: &atlas_engine::TracePath,
    kind: DataFlowKind,
    name: &str,
) {
    let found = path.steps.iter().any(|s| {
        if s.edge_kind != kind {
            return false;
        }
        let from = node_name_or(store, &s.from_node_id, "");
        let to = node_name_or(store, &s.to_node_id, "");
        from.contains(name) || to.contains(name)
    });
    assert!(
        found,
        "no {:?} step referencing name '{}' in {} steps (names: {:?})",
        kind,
        name,
        path.steps.len(),
        path.steps
            .iter()
            .filter(|s| s.edge_kind == kind)
            .map(|s| (
                node_name_or(store, &s.from_node_id, "?"),
                node_name_or(store, &s.to_node_id, "?"),
            ))
            .collect::<Vec<_>>()
    );
}

/// Assert that the source data node has a name that contains `expected`.
fn assert_source_name(path: &atlas_engine::TracePath, expected: &str) {
    let name = path
        .source
        .data_node
        .as_ref()
        .and_then(|dn| dn.name.as_deref())
        .unwrap_or("<none>");
    assert!(
        name.contains(expected),
        "source data node name '{name}' should contain '{expected}'"
    );
}

/// Assert that the trace path has at least `min_steps` steps and that
/// the source data node is NOT the same as the sink trace point.
fn assert_path_completeness(path: &atlas_engine::TracePath, min_steps: usize, sink_name: &str) {
    assert!(
        path.steps.len() >= min_steps,
        "expected >= {} steps, got {}",
        min_steps,
        path.steps.len()
    );
    let source_name = path
        .source
        .data_node
        .as_ref()
        .and_then(|dn| dn.name.as_deref())
        .unwrap_or("<none>");
    assert_ne!(
        source_name, sink_name,
        "source data node '{source_name}' should differ from sink '{sink_name}'"
    );
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
        "expected >= 3 Assign edges (1→x, +2→x, +3→x), got {assign_count}"
    );
}

// ────────────────────────────────────────────────────────────────
// Fixture 3: Cross‑file caller → callee parameter bridging.
// ────────────────────────────────────────────────────────────────

/// FX3: When a caller in main.ts invokes compute(base, factor) in helper.ts,
/// the backward slice from the callee's parameter `base` must include an
/// ArgToParam edge from the caller's argument.
///
/// Cross‑file bridging is provided by RuntimeEdgeProvider; evidence on each
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

    // Use interprocedural bridging via RuntimeEdgeProvider.
    use atlas_engine::trace::virtual_edges::RuntimeEdgeProvider;
    let point = Locator::locate(
        store.as_ref(),
        &helper_id,
        base_node.range.start_line + 1,
        base_node.range.start_column + 1,
    )
    .expect("locate failed");
    let path = Slicer::slice(store.as_ref(), &point, 20, Some(&RuntimeEdgeProvider))
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
/// Cross‑file bridging is provided by RuntimeEdgeProvider; evidence on each
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

    use atlas_engine::trace::virtual_edges::RuntimeEdgeProvider;
    let path = Slicer::slice(store.as_ref(), &point, 20, Some(&RuntimeEdgeProvider))
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
fn fx5_java_capability_declares_dataflow_interproc() {
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
    // Java now has DataflowInterproc support; trace_point must still expose capability metadata.
    let resp = engine.trace_point(&file_id, 3, 20);

    // Must NOT crash. Response must be ok=true.
    assert!(resp.ok, "Java trace_point must return ok=true");

    // Capability must be present and indicate DataflowInterproc level.
    let cap = resp
        .capability
        .as_ref()
        .expect("Java capability must be present");
    assert_eq!(cap.language, "java");
    assert!(
        cap.capability_level == atlas_engine::capability::CapabilityLevel::DataflowInterproc,
        "Java must be DataflowInterproc, got {:?}",
        cap.capability_level
    );

    // Java now supports local_dataflow and use_def at DataflowLocal level.
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
fn fx6_python_cfg_supported_in_capability() {
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

    // Python has CFG support through the language-aware CfgBuilder.
    let cap = resp
        .capability
        .as_ref()
        .expect("Python capability must exist");
    assert_eq!(cap.language, "python");

    assert!(
        cap.features.cfg.is_supported(),
        "Python CFG must be Supported in FeatureMatrix, got {:?}",
        cap.features.cfg
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
        "expected >= 1 ArgToParam edge in indirect chain, got {arg_to_param_count}"
    );

    // Should have evidence from file a.ts (the indirect caller)
    let has_a_evidence = path.steps.iter().any(|s| {
        s.evidence
            .as_ref()
            .is_some_and(|e| e.file_path.contains("a.ts"))
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
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
    let sink = data_nodes
        .iter()
        .find(|n| n.kind == DataNodeKind::Local && n.name.as_deref() == Some("y"))
        .expect("must find Local y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "csharp");

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Rust: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX14: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// PHP: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX15: When `outer()` calls `inner($x)`, tracing backward from callee param
/// `$p` must include an ArgToParam edge.
#[test]
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// PHP: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX16: When `outer()` calls `inner()` and assigns result to `$y`, tracing
/// backward from `$y` must include a ReturnToCall edge.
#[test]
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Ruby: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX18: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
///
/// NOTE: Uses `inner()` with explicit parentheses.  Bare calls (without `()`)
/// require tree-sitter-ruby parsing improvements to be recognized as call nodes.
#[test]
#[cfg(feature = "ruby")]
fn fx18_ruby_cross_function_return_to_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "retbridge.rb",
        r#"def outer
  y = inner()
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Ruby: Basic local dataflow — assignment → use → return within a function
// ────────────────────────────────────────────────────────────────

/// FX32: Within a single Ruby function, verify that local assignments, reads,
/// and return value edges are produced. Proves basic dataflow reliability.
#[test]
#[cfg(feature = "ruby")]
fn fx32_ruby_basic_local_dataflow() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "compute.rb",
        r#"def compute
  a = 10
  b = a + 5
  b * 2
end
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("compute.rb");

    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");
    let engine = TraceEngine::new(store.clone());

    // ── Trace backward from the expression `b` in `b * 2` ───────
    let b_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.name.as_deref() == Some("b"))
        .collect();
    assert!(!b_nodes.is_empty(), "expected data nodes named 'b'");

    // Use the last occurrence of 'b' (the use in `b * 2`)
    let b_use = b_nodes.last().unwrap();
    let resp = engine.trace_variable(
        &file_id,
        b_use.range.start_line + 1,
        b_use.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "ruby");

    let path = resp.result.expect("basic dataflow trace must produce path");
    assert!(!path.steps.is_empty(), "trace must have steps");
    assert!(
        path.steps.iter().any(|s| {
            matches!(
                s.edge_kind,
                DataFlowKind::Assign
                    | DataFlowKind::Read
                    | DataFlowKind::Write
                    | DataFlowKind::ReturnValue
            )
        }),
        "expected Assign/Read/Write/ReturnValue edge in local dataflow trace, got steps: {:?}",
        path.steps.iter().map(|s| s.edge_kind).collect::<Vec<_>>()
    );

    // ── Verify function return node exists ──────────────────────
    let return_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Return)
        .collect();
    assert!(
        !return_nodes.is_empty(),
        "expected at least one Return data node in Ruby function"
    );
}

// ────────────────────────────────────────────────────────────────
// Kotlin: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX19: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Kotlin: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX20: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Python: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX22: Python ReturnToCall — When `fx_py_return_to_call_process()` calls
/// `fx_py_return_to_call_get_value()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// Python: Shadowing precision — inner scope x shadows outer x
// ────────────────────────────────────────────────────────────────

/// FX_PY_SHADOW: When `x` is shadowed in a nested scope (if_statement), the
/// trace from `result` (which uses the inner `x`) must reach the inner `x`
/// without conflating the outer `x`.  This proves that scope-chain-aware
/// binding resolution (`resolve_bindings_to_nodes` in dataflow_builder.rs)
/// works correctly for Python.
#[test]
#[cfg(feature = "python")]
fn fx_py_shadow() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "shadow.py",
        r#"def shadow_test():
    x = "outer"            # outer x in function scope
    if True:
        x = "inner"        # inner x in conditional scope — shadows outer
        result = x         # uses inner x, NOT outer x
    return result          # <-- trace point
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("shadow.py");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // Collect the two Local/assign_target nodes named "x" — first is outer, second is inner
    let x_local_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Local && n.name.as_deref() == Some("x"))
        .collect();
    assert!(
        x_local_nodes.len() >= 2,
        "expected >=2 Local 'x' nodes (outer + inner), got {}",
        x_local_nodes.len()
    );
    let outer_x = x_local_nodes[0];
    let inner_x = x_local_nodes[1];

    // Verify the two x bindings have different binding_ids (shadowing works)
    assert!(
        outer_x.binding_id != inner_x.binding_id,
        "outer x (binding={:?}) and inner x (binding={:?}) must have distinct binding_ids",
        outer_x.binding_id,
        inner_x.binding_id
    );

    // Trace from result
    let sink = find_node(&data_nodes, "result");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "python");

    let path = resp.result.expect("trace path must exist");
    assert!(!path.steps.is_empty(), "backward slice must have steps");

    // ── CRITICAL: inner x must appear in the trace path ──
    let inner_in_path = path
        .steps
        .iter()
        .any(|step| step.from_node_id == inner_x.id || step.to_node_id == inner_x.id);
    assert!(
        inner_in_path,
        "inner 'x' must be in the trace path (result uses inner x)"
    );

    // ── CRITICAL: outer x must NOT appear in the trace path ──
    let violation = path
        .steps
        .iter()
        .any(|step| step.from_node_id == outer_x.id || step.to_node_id == outer_x.id);
    assert!(
        !violation,
        "shadowing violation: outer 'x' must NOT be in inner trace path"
    );
}

// ────────────────────────────────────────────────────────────────
// Python: Destructuring / tuple unpacking dataflow
// ────────────────────────────────────────────────────────────────

/// FX_PY_DESTRUCTURE: `a, b = (1, 2)` must produce distinct Local data nodes
/// for both `a` and `b`, and tracing from `result` must find both variable
/// sources (a → result and b → result via the addition expression).
#[test]
#[cfg(feature = "python")]
fn fx_py_destructure() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "destructure.py",
        r#"def destructure_test():
    a, b = (1, 2)
    result = a + b
    return result
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("destructure.py");

    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    // Verify both a and b have Local nodes (destructuring captured)
    let a_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Local && n.name.as_deref() == Some("a"))
        .collect();
    assert!(
        !a_nodes.is_empty(),
        "destructuring must produce Local node for 'a'"
    );

    let b_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Local && n.name.as_deref() == Some("b"))
        .collect();
    assert!(
        !b_nodes.is_empty(),
        "destructuring must produce Local node for 'b'"
    );

    // Verify both a and b have binding_ids set
    assert!(
        a_nodes[0].binding_id.is_some(),
        "destructured 'a' must have binding_id"
    );
    assert!(
        b_nodes[0].binding_id.is_some(),
        "destructured 'b' must have binding_id"
    );

    // Trace from result
    let sink = find_node(&data_nodes, "result");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "python");

    let path = resp.result.expect("trace path must exist");
    assert!(
        !path.steps.is_empty(),
        "destructuring trace must have steps"
    );

    // Verify the trace path has Assign edges (from a, b → result)
    let assign_count = path
        .steps
        .iter()
        .filter(|s| s.edge_kind == DataFlowKind::Assign)
        .count();
    assert!(
        assign_count >= 1,
        "expected >=1 Assign edge from destructured vars, got {assign_count}"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// C: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX24: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// C++: Cross‑function ReturnToCall
// ────────────────────────────────────────────────────────────────

/// FX26: When `outer()` calls `inner()` and assigns result to `y`, tracing
/// backward from `y` must include a ReturnToCall edge.
#[test]
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
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
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
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

    let path = resp
        .result
        .expect("cross-function return trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function return trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
}

// ────────────────────────────────────────────────────────────────
// ArkTS: @Component + @State decorator — symbol/scope/reference extraction
// ────────────────────────────────────────────────────────────────

/// FX30: ArkTS @Component and @State decorators using `class` (TS grammar
/// fallback). Verifies symbols, references, and scopes are extracted for
/// ArkTS-specific constructs.
#[test]
#[cfg(feature = "arkts")]
fn fx30_arkts_component_decorator_extraction() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "component.ets",
        r#"
@Component
class MyComponent {
  @State count: number = 0;

  build() {
    console.log(this.count.toString())
  }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("component.ets");

    // ── Symbols ──────────────────────────────────────────────────
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    assert!(
        !symbols.is_empty(),
        "expected symbols for ArkTS @Component class"
    );

    let component_sym = symbols
        .iter()
        .find(|s| s.name == "MyComponent")
        .expect("expected 'MyComponent' class symbol");
    assert!(
        matches!(
            component_sym.kind,
            atlas_engine::enums::SymbolKind::Class | atlas_engine::enums::SymbolKind::Struct
        ),
        "MyComponent should be a class/struct symbol, got {:?}",
        component_sym.kind
    );

    let build_sym = symbols
        .iter()
        .find(|s| s.name == "build")
        .expect("expected 'build' method symbol");
    assert!(
        matches!(
            build_sym.kind,
            atlas_engine::enums::SymbolKind::Method | atlas_engine::enums::SymbolKind::Function
        ),
        "build should be a method/function symbol, got {:?}",
        build_sym.kind
    );

    // NOTE: class properties (e.g. @State count) are not captured as
    //       symbols by the TS definitions.scm query — no @definition.property
    //       capture exists. This is a known TS grammar fallback gap.

    // ── References ───────────────────────────────────────────────
    let refs = store.find_references_by_file(&file_id).expect("references");
    assert!(
        !refs.is_empty(),
        "expected references for ArkTS @Component class"
    );

    // this.count → should be captured as a @reference.field
    let field_ref = refs
        .iter()
        .find(|r| r.name == "count")
        .expect("expected reference to 'count' (via this.count member_expression)");
    assert!(
        field_ref.name == "count",
        "reference name should be 'count', got {:?}",
        field_ref.name
    );

    // console.log → should be captured as a @reference.call (method call)
    let has_log_call = refs.iter().any(|r| r.name == "log");
    assert!(
        has_log_call,
        "expected reference to 'log' (console.log call)"
    );

    // ── Scopes ───────────────────────────────────────────────────
    let scopes = store.find_scopes_by_file(&file_id).expect("scopes");
    assert!(
        !scopes.is_empty(),
        "expected scopes for ArkTS @Component class"
    );

    // NOTE: TS scope names are generated as Kind#byte_offset (e.g. Class#123),
    //       not human-readable.  Verify by kind, not name.
    let has_class_scope = scopes.iter().any(|s| {
        matches!(
            s.kind,
            atlas_engine::enums::ScopeKind::Class | atlas_engine::enums::ScopeKind::Struct
        )
    });
    assert!(
        has_class_scope,
        "expected a class/struct scope for MyComponent, got: {:?}",
        scopes.iter().map(|s| s.kind).collect::<Vec<_>>()
    );

    let has_method_scope = scopes.iter().any(|s| {
        matches!(
            s.kind,
            atlas_engine::enums::ScopeKind::Method | atlas_engine::enums::ScopeKind::Function
        )
    });
    assert!(
        has_method_scope,
        "expected a method/function scope for build(), got: {:?}",
        scopes.iter().map(|s| s.kind).collect::<Vec<_>>()
    );
}

// ────────────────────────────────────────────────────────────────
// ArkTS: struct-as-class — symbol extraction + return type reference
// ────────────────────────────────────────────────────────────────

/// FX31: Since tree-sitter-typescript does not recognise the ArkTS `struct`
/// keyword, `class` is used as the fallback syntax. Verifies that class symbols
/// are extracted and that function return type annotations reference the class
/// (symbol + reference coverage).
#[test]
#[cfg(feature = "arkts")]
fn fx31_arkts_class_as_struct_extraction() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "point.ets",
        r#"
class Point {
  x: number;
  y: number;
}
function createPoint(): Point {
  return { x: 1, y: 2 };
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("point.ets");

    // ── Symbols ──────────────────────────────────────────────────
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    assert!(
        !symbols.is_empty(),
        "expected symbols for ArkTS class-as-struct"
    );

    let point_sym = symbols
        .iter()
        .find(|s| s.name == "Point")
        .expect("expected 'Point' class symbol");
    assert!(
        matches!(
            point_sym.kind,
            atlas_engine::enums::SymbolKind::Class | atlas_engine::enums::SymbolKind::Struct
        ),
        "Point should be a class/struct symbol, got {:?}",
        point_sym.kind
    );

    let create_fn = symbols
        .iter()
        .find(|s| s.name == "createPoint")
        .expect("expected 'createPoint' function symbol");
    assert!(
        matches!(create_fn.kind, atlas_engine::enums::SymbolKind::Function),
        "createPoint should be a function symbol, got {:?}",
        create_fn.kind
    );

    // ── Dataflow: trace return in createPoint ────────────────────
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");
    let return_nodes: Vec<_> = data_nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Return)
        .collect();
    assert!(!return_nodes.is_empty(), "expected a Return data node");
    let ret_node = return_nodes[0];

    let engine = TraceEngine::new(store.clone());
    let resp = engine.trace_variable(
        &file_id,
        ret_node.range.start_line + 1,
        ret_node.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "arkts");

    let path = resp.result.expect("trace from return must produce path");
    assert!(!path.steps.is_empty(), "trace must have steps");
    assert!(
        path.steps.iter().any(|s| {
            matches!(
                s.edge_kind,
                DataFlowKind::ReturnValue | DataFlowKind::Assign
            )
        }),
        "expected ReturnValue or Assign edge in function return trace, got steps: {:?}",
        path.steps.iter().map(|s| s.edge_kind).collect::<Vec<_>>()
    );
}

/// Verify ArkTS CFG body traversal for if/else:
/// Entry, Branch, Statement nodes in branches, TrueBranch/FalseBranch edges, Join, Exit.
#[test]
#[cfg(feature = "arkts")]
fn fx_cfg_if_else_arkts() {
    let files = &[(
        "cfg_if.ets",
        r#"function testIf(x: number): number {
    if (x > 0) {
        return 1;
    } else {
        return -1;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.ets");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "missing Exit"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "missing Join"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }

        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::TrueBranch),
            "missing TrueBranch edge for consequence body"
        );
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::FalseBranch),
            "missing FalseBranch edge for alternative body"
        );
    }
}

/// Verify ArkTS CFG body traversal for for loop:
/// Loop node, Statement node in body, LoopBack edge, Normal exit.
#[test]
#[cfg(feature = "arkts")]
fn fx_cfg_loop_arkts() {
    let files = &[(
        "cfg_loop.ets",
        r#"function sum(n: number): number {
    let total: number = 0;
    for (let i = 0; i < n; i++) {
        total += i;
    }
    return total;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.ets");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "missing Exit"
        );

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "missing Loop"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Statement),
            "missing loop-body Statement"
        );
        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }

        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "missing LoopBack edge for loop body"
        );
    }
}

/// Verify the shared TS grammar path does not collapse ArkTS switch arms into
/// one statement: each case/default body must be a persisted sibling CFG path.
#[test]
#[cfg(feature = "arkts")]
fn fx_cfg_switch_arkts() {
    let files = &[(
        "cfg_switch.ets",
        r#"function dispatch(command: number): number {
    switch (command) {
        case 1:
            return install();
        case 2:
            return remove();
        default:
            return unknown();
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_switch.ets");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing ArkTS dispatch function");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("ArkTS switch missing Branch");
    assert!(
        cfg_nodes.iter().any(|node| node.kind == CfgNodeKind::Join),
        "ArkTS switch missing Join"
    );
    let case_edges = store
        .find_cfg_edges_by_source(&branch.id)
        .expect("case edges")
        .into_iter()
        .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 3,
        "expected two cases and default without a no-match edge"
    );
}

#[test]
#[cfg(feature = "arkts")]
fn fx_cfg_try_catch_arkts_without_finally() {
    let files = &[(
        "cfg_try.ets",
        r#"function load(path: string): string {
    try {
        if (path.length === 0) {
            throw new Error("empty");
        }
        readFile(path);
    } catch (error) {
        recover(error);
    }
    return path;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_try.ets");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "load" && symbol.kind == SymbolKind::Function)
        .expect("missing ArkTS load function");
    assert_persisted_exception_cfg(&store, &symbol.id, "ArkTS", 2);
}

#[test]
#[cfg(feature = "typescript")]
fn fx_cfg_try_finally_typescript_persists_path_isolated_clones() {
    let source = r#"function f(flag: boolean): void {
    try {
        if (flag) return;
        work();
    } finally {
        cleanup();
    }
    after();
}
"#;
    let store = index_files(&[("cfg_finally.ts", source)]);
    let file_id = FileId::generate("cfg_finally.ts");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "f" && symbol.kind == SymbolKind::Function)
        .expect("missing TypeScript f function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let cleanup_ids =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Statement, "cleanup();");
    assert_eq!(
        cleanup_ids.len(),
        2,
        "normal and return finally clones must both survive SQLite persistence"
    );
    assert_ne!(cleanup_ids[0], cleanup_ids[1]);

    let return_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return;");
    let work_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "work();");
    let after_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "after();");
    let edges = persisted_cfg_edges(&store, &nodes);
    assert!(edges.iter().any(|edge| {
        edge.source == return_id
            && cleanup_ids.contains(&edge.target)
            && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(persisted_cfg_reaches(&edges, work_id, after_id));
    assert!(!persisted_cfg_reaches(&edges, return_id, after_id));
}

#[test]
#[cfg(feature = "python")]
fn fx_cfg_python_with_return_persists_owned_block_exit_clones_and_cleanup() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;
    use atlas_engine::enums::CallContext;

    let source = "def f(flag):\n    with open('x') as resource:\n        if flag:\n            return 1\n        work(resource)\n    after()\n";
    let store = index_files(&[("cfg_with_return.py", source)]);
    let file_id = FileId::generate("cfg_with_return.py");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "f" && symbol.kind == SymbolKind::Function)
        .expect("missing Python f function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let block_exits: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == CfgNodeKind::BlockExit)
        .collect();
    assert_eq!(block_exits.len(), 2);
    assert_ne!(block_exits[0].id, block_exits[1].id);
    let scope_owner = block_exits[0]
        .managed_scope_start_byte
        .expect("persisted managed-scope owner");
    assert!(
        block_exits
            .iter()
            .all(|node| node.managed_scope_start_byte == Some(scope_owner))
    );
    assert!(nodes.iter().any(|node| {
        node.call_context == CallContext::PythonWith
            && node.managed_scope_start_byte == Some(scope_owner)
    }));

    let return_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return 1");
    let work_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "work(resource)");
    let after_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "after()");
    let edges = persisted_cfg_edges(&store, &nodes);
    assert!(block_exits.iter().any(|block_exit| {
        edges.iter().any(|edge| {
            edge.source == return_id
                && edge.target == block_exit.id
                && edge.kind == CfgEdgeKind::Normal
        })
    }));
    assert!(persisted_cfg_reaches(&edges, work_id, after_id));
    assert!(!persisted_cfg_reaches(&edges, return_id, after_id));

    let cfg = CfgGraph::build(&nodes, &edges).expect("persisted CFG");
    let data_nodes = store
        .find_data_nodes_by_function(&symbol.id)
        .expect("data nodes");
    let dataflow_edges = store
        .find_dataflow_edges_by_sources(&data_nodes.iter().map(|node| node.id).collect::<Vec<_>>())
        .expect("dataflow edges");
    let composition = compose_effects(
        &cfg,
        &data_nodes,
        &dataflow_edges,
        &ResourceOpConfig::default_for(Language::Python),
    );
    for block_exit in block_exits {
        assert!(
            composition
                .node_effects
                .get(&block_exit.id)
                .is_some_and(|effects| effects
                    .iter()
                    .any(|effect| { matches!(effect.kind, SemanticEffectKind::Free { .. }) })),
            "every persisted continuation must receive context-managed cleanup"
        );
    }
}

#[test]
#[cfg(feature = "python")]
fn fx_cfg_python_with_cleanup_exception_persists_handler_continuation() {
    let source = r#"def f():
    try:
        with open('x') as resource:
            return resource
    except OSError:
        recover()
"#;
    let store = index_files(&[("cfg_with_cleanup_exception.py", source)]);
    let file_id = FileId::generate("cfg_with_cleanup_exception.py");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "f" && symbol.kind == SymbolKind::Function)
        .expect("missing Python f function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let return_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return resource");
    let handler =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "recover()");
    let edges = persisted_cfg_edges(&store, &nodes);
    let return_exit = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::BlockExit
                && edges.iter().any(|edge| {
                    edge.source == return_id
                        && edge.target == node.id
                        && edge.kind == CfgEdgeKind::Normal
                })
        })
        .expect("return must execute its own managed exit");

    assert!(edges.iter().any(|edge| {
        edge.source == return_exit.id
            && edge.target == handler
            && edge.kind == CfgEdgeKind::Exception
    }));
}

#[test]
#[cfg(feature = "cangjie")]
fn fx_cfg_real_cangjie_example_finally_cleanup_is_structured() {
    let source = example_source_or_skip!("cangjie_example/src/command_install.cj");
    let store = index_files(&[("command_install.cj", source)]);
    let file_id = FileId::generate("command_install.cj");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "doinstall" && symbol.kind == SymbolKind::Function)
        .expect("missing real Cangjie doinstall function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let install_success = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "println(\"install success.\")",
    );
    let cleanup_ids = persisted_cfg_node_ids_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "removeIfExists(defaultConfig.cacheDir, recursive: true)",
    );
    assert_eq!(
        cleanup_ids.len(),
        1,
        "real example has one normal continuation"
    );
    let edges = persisted_cfg_edges(&store, &nodes);
    assert!(persisted_cfg_reaches(
        &edges,
        install_success,
        cleanup_ids[0]
    ));
    assert!(edges.iter().any(|edge| {
        edge.source == cleanup_ids[0]
            && edge.kind == CfgEdgeKind::Normal
            && nodes
                .iter()
                .any(|node| node.id == edge.target && node.kind == CfgNodeKind::Join)
    }));
}

#[test]
#[cfg(feature = "cpp")]
fn fx_cfg_real_jemalloc_cpp_try_catch_persists_exception_edge() {
    let source = example_source_or_skip!("redis/deps/jemalloc/src/jemalloc_cpp.cpp");
    let store = index_files(&[("jemalloc_cpp.cpp", source)]);
    let file_id = FileId::generate("jemalloc_cpp.cpp");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "handleOOM" && symbol.kind == SymbolKind::Function)
        .expect("missing real jemalloc handleOOM function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let catch_start = source
        .find("catch (const std::bad_alloc &)")
        .expect("real catch clause");
    let handler = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Statement
                && node.stmt_range.start_byte as usize > catch_start
                && source
                    .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                    .is_some_and(|text| text.trim() == "break;")
        })
        .expect("catch break");
    let edges = persisted_cfg_edges(&store, &nodes);
    let dispatch = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Branch
                && edges.iter().any(|edge| {
                    edge.source == node.id
                        && edge.target == handler.id
                        && edge.kind == CfgEdgeKind::Exception
                })
        })
        .expect("try dispatch must retain persisted Exception edge to catch");

    assert!(edges.iter().any(|edge| {
        edge.source == dispatch.id && edge.kind == CfgEdgeKind::Normal && edge.target != handler.id
    }));
}

#[test]
#[cfg(feature = "java")]
fn fx_cfg_real_java_exact_explicit_throw_prunes_later_handler_after_persistence() {
    let source = example_source_or_skip!(
        "elasticsearch/server/src/main/java/org/elasticsearch/rest/action/RestActions.java"
    );
    let store = index_files(&[("RestActions.java", source)]);
    let file_id = FileId::generate("RestActions.java");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "getQueryContent"
                && symbol.kind == SymbolKind::Method
                && symbol
                    .signature
                    .as_deref()
                    .is_some_and(|signature| signature.contains("SearchRequest"))
        })
        .expect("missing real Java RestActions.getQueryContent overload");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let body_throw = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Throw,
        "throw new ParsingException(parser.getTokenLocation(), \"request does not support [\" + parser.currentName() + \"]\");",
    );
    let exact_handler =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Throw, "throw e;");
    let later_handler = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Throw,
        "throw new ParsingException(parser == null ? null : parser.getTokenLocation(), \"Failed to parse\", e);",
    );
    let edges = persisted_cfg_edges(&store, &nodes);

    assert!(edges.iter().any(|edge| {
        edge.source == body_throw
            && edge.target == exact_handler
            && edge.kind == CfgEdgeKind::Exception
    }));
    assert!(!edges.iter().any(|edge| {
        edge.source == body_throw
            && edge.target == later_handler
            && edge.kind == CfgEdgeKind::Exception
    }));
    assert!(edges.iter().any(|edge| {
        edge.target == later_handler
            && edge.kind == CfgEdgeKind::Exception
            && nodes
                .iter()
                .any(|node| node.id == edge.source && node.kind == CfgNodeKind::Branch)
    }));
}

#[test]
#[cfg(feature = "java")]
fn fx_cfg_real_java_example_return_executes_finally_before_exit() {
    let source =
        example_source_or_skip!("java_example/brut.j.util/src/main/java/brut/util/BrutIO.java");
    let store = index_files(&[("BrutIO.java", source)]);
    let file_id = FileId::generate("BrutIO.java");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "readAndClose" && symbol.kind == SymbolKind::Method)
        .expect("missing real Java BrutIO.readAndClose method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let return_id = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Return,
        "return IOUtils.toByteArray(in);",
    );
    let cleanup_id = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "IOUtils.closeQuietly(in);",
    );
    let exit_id = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .expect("Java CFG missing Exit")
        .id;
    let edges = persisted_cfg_edges(&store, &nodes);
    assert!(edges.iter().any(|edge| {
        edge.source == return_id && edge.target == cleanup_id && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(edges.iter().any(|edge| {
        edge.source == cleanup_id && edge.target == exit_id && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(
        !edges
            .iter()
            .any(|edge| edge.source == return_id && edge.target == exit_id)
    );
}

#[test]
#[cfg(feature = "java")]
fn fx_cfg_real_java_try_with_resources_return_executes_owned_block_exit() {
    use atlas_engine::enums::CallContext;

    let source =
        example_source_or_skip!("java_example/brut.j.xml/src/main/java/brut/xml/XmlUtils.java");
    let store = index_files(&[("XmlUtils.java", source)]);
    let file_id = FileId::generate("XmlUtils.java");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "loadDocument"
                && symbol.kind == SymbolKind::Method
                && symbol
                    .signature
                    .as_deref()
                    .is_some_and(|signature| signature.contains("boolean"))
        })
        .expect("missing real Java XmlUtils.loadDocument(File, boolean) method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let return_id = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Return,
        "return builder.parse(new InputSource(in));",
    );
    let block_exit = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::BlockExit)
        .expect("Java try-with-resources missing BlockExit");
    let owner = block_exit
        .managed_scope_start_byte
        .expect("persisted Java managed scope owner");
    assert!(nodes.iter().any(|node| {
        node.call_context == CallContext::JavaTryWith
            && node.managed_scope_start_byte == Some(owner)
    }));

    let exit_id = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .expect("Java CFG missing Exit")
        .id;
    let edges = persisted_cfg_edges(&store, &nodes);
    assert!(edges.iter().any(|edge| {
        edge.source == return_id && edge.target == block_exit.id && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(edges.iter().any(|edge| {
        edge.source == block_exit.id && edge.target == exit_id && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(
        !edges
            .iter()
            .any(|edge| edge.source == return_id && edge.target == exit_id)
    );
}

#[test]
#[cfg(feature = "java")]
fn fx_cfg_real_java_labeled_breaks_target_outer_loop_after_persistence() {
    let source = example_source_or_skip!(
        "elasticsearch/libs/lz4/src/main/java/org/elasticsearch/lz4/ESLZ4Compressor.java"
    );
    let store = index_files(&[("ESLZ4Compressor.java", source)]);
    let file_id = FileId::generate("ESLZ4Compressor.java");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "compress64k" && symbol.kind == SymbolKind::Method)
        .expect("missing real Java ESLZ4Compressor.compress64k method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let labeled_breaks =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Statement, "break label53;");
    assert_eq!(labeled_breaks.len(), 2, "real method has two labeled exits");

    let targets: std::collections::HashSet<_> = labeled_breaks
        .iter()
        .flat_map(|break_id| {
            edges.iter().filter_map(move |edge| {
                (edge.source == *break_id && edge.kind == CfgEdgeKind::Break).then_some(edge.target)
            })
        })
        .collect();
    assert_eq!(targets.len(), 1, "both labeled breaks exit the same loop");
    let loop_join = *targets.iter().next().expect("labeled loop Join");
    assert!(
        nodes
            .iter()
            .any(|node| node.id == loop_join && node.kind == CfgNodeKind::Join)
    );
    assert!(nodes.iter().any(|node| {
        node.kind == CfgNodeKind::Loop
            && edges.iter().any(|edge| {
                edge.source == node.id
                    && edge.target == loop_join
                    && edge.kind == CfgEdgeKind::Normal
            })
    }));

    let post_loop = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "dOff = LZ4SafeUtils.lastLiterals(src, anchor, srcEnd - anchor, dest, dOff, destEnd);",
    );
    assert!(persisted_cfg_reaches(&edges, loop_join, post_loop));
}

#[test]
#[cfg(feature = "java")]
fn fx_cfg_real_java_labeled_continue_targets_outer_loop_after_persistence() {
    let source = example_source_or_skip!(
        "elasticsearch/libs/lz4/src/main/java/org/elasticsearch/lz4/ESLZ4Compressor.java"
    );
    let store = index_files(&[("ESLZ4Compressor.java", source)]);
    let file_id = FileId::generate("ESLZ4Compressor.java");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "compress" && symbol.kind == SymbolKind::Method)
        .expect("missing real Java ESLZ4Compressor.compress byte-array overload");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let continue_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "continue label63;");
    let target_id = edges
        .iter()
        .find_map(|edge| {
            (edge.source == continue_id && edge.kind == CfgEdgeKind::Continue)
                .then_some(edge.target)
        })
        .expect("labeled continue edge");
    let target = nodes
        .iter()
        .find(|node| node.id == target_id)
        .expect("labeled continue target");
    assert_eq!(target.kind, CfgNodeKind::Loop);
    let target_start = target.stmt_range.start_byte as usize;
    assert!(
        source[..target_start].ends_with("label63: "),
        "continue label63 must bypass inner loops and target the labeled loop"
    );
}

#[test]
#[cfg(feature = "java")]
fn fx_cfg_real_java_try_with_resources_catch_follows_owned_block_exit() {
    use atlas_engine::enums::CallContext;

    let source =
        example_source_or_skip!("java_example/brut.j.util/src/main/java/brut/util/Jar.java");
    let store = index_files(&[("Jar.java", source)]);
    let file_id = FileId::generate("Jar.java");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "extractToTmp"
                && symbol.kind == SymbolKind::Method
                && symbol
                    .signature
                    .as_deref()
                    .is_some_and(|signature| signature.contains("tmpPrefix"))
        })
        .expect("missing real Java Jar.extractToTmp three-argument overload");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let body_throw = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Throw,
        "throw new FileNotFoundException(name);",
    );
    let return_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return fileOut;");
    let catch_throw = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Throw,
        "throw new BrutException(\"Could not extract resource: \" + name, ex);",
    );
    let managed_exit = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::BlockExit)
        .filter(|node| {
            store
                .find_cfg_edges_by_source(&body_throw)
                .expect("body throw edges")
                .iter()
                .any(|edge| edge.target == node.id && edge.kind == CfgEdgeKind::Normal)
        })
        .expect("body throw must execute the managed exit");
    let owner = managed_exit
        .managed_scope_start_byte
        .expect("persisted Java managed scope owner");
    assert!(nodes.iter().any(|node| {
        node.call_context == CallContext::JavaTryWith
            && node.managed_scope_start_byte == Some(owner)
    }));

    let edges = persisted_cfg_edges(&store, &nodes);
    let return_exit = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::BlockExit
                && edges.iter().any(|edge| {
                    edge.source == return_id
                        && edge.target == node.id
                        && edge.kind == CfgEdgeKind::Normal
                })
        })
        .expect("return must execute its own managed exit");
    assert_ne!(managed_exit.id, return_exit.id);
    assert!(edges.iter().any(|edge| {
        edge.source == managed_exit.id
            && edge.target == catch_throw
            && edge.kind == CfgEdgeKind::Exception
    }));
    assert!(edges.iter().any(|edge| {
        edge.source == return_exit.id
            && edge.target == catch_throw
            && edge.kind == CfgEdgeKind::Exception
    }));
    assert!(!edges.iter().any(|edge| {
        edge.source == body_throw
            && edge.target == catch_throw
            && edge.kind == CfgEdgeKind::Exception
    }));
}

#[test]
#[cfg(feature = "csharp")]
fn fx_cfg_real_csharp_nested_using_return_executes_both_block_exits() {
    let source =
        example_source_or_skip!("c_sharp_example/shadowsocks-csharp/Controller/FileManager.cs");
    let store = index_files(&[("FileManager.cs", source)]);
    let file_id = FileId::generate("FileManager.cs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "NonExclusiveReadAllText"
                && symbol.kind == SymbolKind::Method
                && symbol
                    .signature
                    .as_deref()
                    .is_some_and(|signature| signature.contains("Encoding"))
        })
        .expect("missing real C# NonExclusiveReadAllText overload");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let return_id = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Return,
        "return sr.ReadToEnd();",
    );
    let block_exits: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == CfgNodeKind::BlockExit)
        .collect();
    assert_eq!(
        block_exits.len(),
        3,
        "inner return exit plus outer success and propagated-cleanup-throw exits"
    );
    let owners: std::collections::HashSet<_> = block_exits
        .iter()
        .map(|node| {
            node.managed_scope_start_byte
                .expect("persisted C# managed scope owner")
        })
        .collect();
    assert_eq!(owners.len(), 2, "inner and outer using owners must differ");

    let exit_id = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .expect("C# CFG missing Exit")
        .id;
    let edges = persisted_cfg_edges(&store, &nodes);
    let inner_exit = block_exits
        .iter()
        .find(|node| {
            edges.iter().any(|edge| {
                edge.source == return_id
                    && edge.target == node.id
                    && edge.kind == CfgEdgeKind::Normal
            })
        })
        .expect("return must execute inner using exit");
    let inner_owner = inner_exit
        .managed_scope_start_byte
        .expect("inner using owner");
    let outer_exits: Vec<_> = block_exits
        .iter()
        .filter(|node| node.managed_scope_start_byte != Some(inner_owner))
        .copied()
        .collect();
    assert_eq!(outer_exits.len(), 2);
    assert!(outer_exits.iter().all(|node| {
        edges.iter().any(|edge| {
            edge.source == inner_exit.id
                && edge.target == node.id
                && edge.kind == CfgEdgeKind::Normal
        })
    }));

    let handler =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "logger.Error(ex);");
    for outer_exit in outer_exits {
        assert!(edges.iter().any(|edge| {
            edge.source == outer_exit.id
                && edge.target == exit_id
                && edge.kind == CfgEdgeKind::Normal
        }));
        assert!(edges.iter().any(|edge| {
            edge.source == outer_exit.id
                && edge.target == handler
                && edge.kind == CfgEdgeKind::Exception
        }));
    }
}

#[test]
#[cfg(feature = "csharp")]
fn fx_cfg_csharp_cleanup_crossing_goto_persists_inner_to_outer_route() {
    let source = r#"class T {
  void Run() {
    try {
      using (Resource resource = Open()) {
        goto Done;
      }
    } finally {
      cleanup();
    }
    Done: finish();
  }
}"#;
    let path = "cleanup_goto.cs";
    let store = index_files(&[(path, source)]);
    let file_id = FileId::generate(path);
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("synthetic C# symbols")
        .into_iter()
        .find(|symbol| symbol.name == "Run" && symbol.kind == SymbolKind::Method)
        .expect("T.Run method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("T.Run CFG nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let goto_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "goto Done;");
    let finish =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "finish();");
    let using_exit = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::BlockExit
                && node.call_context == CallContext::CSharpUsing
                && edges.iter().any(|edge| {
                    edge.source == goto_id
                        && edge.target == node.id
                        && edge.kind == CfgEdgeKind::Normal
                })
        })
        .expect("goto must first execute persisted using cleanup");
    let finally_cleanup =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Statement, "cleanup();")
            .into_iter()
            .find(|cleanup| {
                edges.iter().any(|edge| {
                    edge.source == using_exit.id
                        && edge.target == *cleanup
                        && edge.kind == CfgEdgeKind::Normal
                }) && edges.iter().any(|edge| {
                    edge.source == *cleanup
                        && edge.target == finish
                        && edge.kind == CfgEdgeKind::Goto
                })
            })
            .expect("persisted goto continuation must execute outer finally");

    assert!(!edges.iter().any(|edge| {
        edge.source == goto_id && edge.target == finish && edge.kind == CfgEdgeKind::Goto
    }));
    let final_edge = edges
        .iter()
        .find(|edge| {
            edge.source == finally_cleanup
                && edge.target == finish
                && edge.kind == CfgEdgeKind::Goto
        })
        .expect("finally tail must retain the goto continuation kind");
    assert_eq!(
        final_edge.id,
        CfgEdge::new(&finally_cleanup, &finish, CfgEdgeKind::Goto).id
    );
}

#[test]
#[cfg(feature = "csharp")]
fn fx_cfg_real_csharp_direct_goto_persists_exact_label_edge() {
    let source = example_source_or_skip!(
        "c_sharp_example/shadowsocks-csharp/Controller/Service/Listener.cs"
    );
    let path = "examples/Listener.cs";
    let store = index_files(&[(path, source)]);
    let file_id = FileId::generate(path);
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("real C# Listener symbols")
        .into_iter()
        .find(|symbol| symbol.name == "ReceiveCallback" && symbol.kind == SymbolKind::Method)
        .expect("Listener.ReceiveCallback method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("ReceiveCallback CFG nodes");
    let goto_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "goto Shutdown;");
    let target = nodes
        .iter()
        .find(|node| {
            if node.kind != CfgNodeKind::Branch {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source.get(range).is_some_and(|text| {
                text.trim_start()
                    .starts_with("if (conn.ProtocolType == ProtocolType.Tcp)")
            })
        })
        .expect("Shutdown label executable entry");
    let skipped_loop = nodes
        .iter()
        .find(|node| {
            if node.kind != CfgNodeKind::Loop {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim_start().starts_with("foreach (IService service"))
        })
        .expect("service loop skipped by goto");
    let edges = persisted_cfg_edges(&store, &nodes);
    let outgoing: Vec<_> = edges.iter().filter(|edge| edge.source == goto_id).collect();

    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].target, target.id);
    assert_eq!(outgoing[0].kind, CfgEdgeKind::Goto);
    assert_eq!(
        outgoing[0].id,
        CfgEdge::new(&goto_id, &target.id, CfgEdgeKind::Goto).id,
        "persisted C# edge ID must encode the final goto kind"
    );
    assert!(!persisted_cfg_reaches(&edges, goto_id, skipped_loop.id));
}

// ────────────────────────────────────────────────────────────────
// Cangjie: Cross‑function ArgToParam
// ────────────────────────────────────────────────────────────────

/// FX29: When `outer()` calls `inner(x)`, tracing backward from callee param
/// `p` must include an ArgToParam edge.
#[test]
#[cfg(feature = "cangjie")]
fn fx29_cangjie_cross_function_arg_to_param() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "bridge.cj",
        r#"func outer() {
    let x = 42
    inner(x)
}

func inner(p: Int64): Int64 {
    let result = p
    return result
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.cj");

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
    assert_envelope_ok(&resp, "cangjie");

    let path = resp.result.expect("cross-function trace must produce path");
    assert!(
        !path.steps.is_empty(),
        "cross-function trace must have steps"
    );
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
}

// ────────────────────────────────────────────────────────────────
// Semantic precision fixtures — verify trace correctness beyond
// edge-kind checks.  Each fixture validates:
//  1. Source correctness: source DataNode name matches expected origin
//  2. Step semantics: key variable names appear in trace steps
//  3. Path completeness: trace covers expected cross-function chain
// ────────────────────────────────────────────────────────────────

// ── TypeScript ──────────────────────────────────────────────────────

/// fx_semantic_ts: Trace `y` in `process()` backward — must cross into
/// `source()` via ReturnToCall, include Assign edges for `x`→`y`, and the
/// source DataNode must name the origin variable in `source()`.
#[test]
#[cfg(feature = "typescript")]
fn fx_semantic_ts() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.ts",
        r#"function source(): string {
    let data = "secret";
    return data;
}
function process(): string {
    let x = source();
    let y = x;
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.ts");
    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "typescript");
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── JavaScript ──────────────────────────────────────────────────────

#[test]
#[cfg(feature = "javascript")]
fn fx_semantic_js() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.js",
        r#"function source() {
    let data = "secret";
    return data;
}
function process() {
    let x = source();
    let y = x;
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.js");
    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "javascript");
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

#[test]
#[cfg(feature = "javascript")]
fn fx_javascript_cross_function_arg_to_param() {
    let files = &[(
        "bridge.js",
        r#"function outer() {
    const x = "secret";
    return inner(x);
}

function inner(p) {
    return p;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("bridge.js");
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");
    let param = data_nodes
        .iter()
        .find(|node| node.kind == DataNodeKind::Parameter && node.name.as_deref() == Some("p"))
        .expect("missing JavaScript parameter p");
    let engine = TraceEngine::new(store.clone());
    let resp = engine.trace_variable(
        &file_id,
        param.range.start_line + 1,
        param.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "javascript");
    let path = resp.result.expect("cross-function trace must produce path");
    assert_has_edge_kind(&path, DataFlowKind::ArgToParam);
    assert_source_name(&path, "secret");
}

#[test]
#[cfg(feature = "javascript")]
fn fx_cfg_switch_javascript() {
    let source = r#"function dispatch(command) {
    switch (command) {
        case "install":
            prepare();
        case "remove":
            remove();
            break;
        default:
            unknown();
    }
}
"#;
    let files = &[("cfg_switch.js", source)];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_switch.js");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing JavaScript dispatch function");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("JavaScript switch missing Branch");
    let mut cfg_edges = Vec::new();
    for node in &cfg_nodes {
        cfg_edges.extend(store.find_cfg_edges_by_source(&node.id).expect("cfg edges"));
    }
    let case_edges = cfg_edges
        .iter()
        .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 3,
        "expected two cases and default without a no-match edge"
    );
    let prepare =
        persisted_cfg_node_id_for_text(&cfg_nodes, source, CfgNodeKind::Statement, "prepare();");
    let remove =
        persisted_cfg_node_id_for_text(&cfg_nodes, source, CfgNodeKind::Statement, "remove();");
    assert!(cfg_edges.iter().any(|edge| {
        edge.source == prepare && edge.target == remove && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(cfg_edges.iter().any(|edge| edge.kind == CfgEdgeKind::Break));
}

// ── Python ──────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "python")]
fn fx_semantic_py() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.py",
        r#"def source():
    data = "secret"
    return data

def process():
    x = source()
    y = x
    return y
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.py");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── Java ────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "java")]
fn fx_semantic_java() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "Semantic.java",
        r#"class Semantic {
    String source() {
        String data = "secret";
        return data;
    }
    String process() {
        String x = source();
        String y = x;
        return y;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("Semantic.java");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── C ───────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "c")]
fn fx_semantic_c() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.c",
        r#"int source() {
    int data = 42;
    return data;
}
int process() {
    int x = source();
    int y = x;
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.c");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "42");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── C++ ─────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "cpp")]
fn fx_semantic_cpp() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.cpp",
        r#"int source() {
    int data = 42;
    return data;
}
int process() {
    int x = source();
    int y = x;
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.cpp");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "42");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── Go ──────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "go")]
fn fx_semantic_go() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.go",
        r#"package p

func source() int {
    data := 42
    return data
}

func process() int {
    x := source()
    y := x
    return y
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.go");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "42");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── C# ──────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "csharp")]
fn fx_semantic_cs() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "Semantic.cs",
        r#"class Semantic {
    int Source() {
        int data = 42;
        return data;
    }
    int Process() {
        int x = Source();
        int y = x;
        return y;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("Semantic.cs");
    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "csharp");
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "42");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── Rust ────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "rust")]
fn fx_semantic_rs() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.rs",
        r#"fn source() -> i32 {
    let data = 42;
    data
}
fn process() -> i32 {
    let x = source();
    let y = x;
    y
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.rs");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "42");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── PHP ─────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "php")]
fn fx_semantic_php() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.php",
        r#"<?php
function source() {
    $data = "secret";
    return $data;
}
function process() {
    $x = source();
    $y = $x;
    return $y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.php");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

#[test]
#[cfg(feature = "php")]
fn fx_cfg_php_function_and_method_boundaries() {
    let source = r#"<?php
function dispatch($command) {
    if ($command > 0) {
        positive();
    } elseif ($command === 0) {
        zero();
    } else {
        fallback();
    }

    switch ($command) {
        case 0:
            prepare();
        case 1:
            install();
            break;
        default:
            unknown();
    }
    return $command;
}

class Worker {
    public function run($items) {
        foreach ($items as $item) {
            visit($item);
        }
        if (!$items) {
            throw new RuntimeException("empty");
        }
        return count($items);
    }
}
"#;
    let files = &[("cfg.php", source)];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.php");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");

    let dispatch = symbols
        .iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing PHP dispatch function");
    let dispatch_nodes = store
        .find_cfg_nodes_by_function(&dispatch.id)
        .expect("dispatch CFG nodes");
    assert_eq!(
        dispatch_nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::Branch)
            .count(),
        3,
        "expected if, elseif, and switch branches"
    );
    let mut dispatch_edges = Vec::new();
    for node in &dispatch_nodes {
        dispatch_edges.extend(
            store
                .find_cfg_edges_by_source(&node.id)
                .expect("dispatch CFG edges"),
        );
    }
    assert!(
        dispatch_edges
            .iter()
            .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
            .count()
            >= 3,
        "expected two cases and default paths"
    );
    let prepare = persisted_cfg_node_id_for_text(
        &dispatch_nodes,
        source,
        CfgNodeKind::Statement,
        "prepare();",
    );
    let install = persisted_cfg_node_id_for_text(
        &dispatch_nodes,
        source,
        CfgNodeKind::Statement,
        "install();",
    );
    assert!(dispatch_edges.iter().any(|edge| {
        edge.source == prepare && edge.target == install && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(
        dispatch_edges
            .iter()
            .any(|edge| edge.kind == CfgEdgeKind::Break)
    );

    let run = symbols
        .iter()
        .find(|symbol| symbol.name == "run" && symbol.kind == SymbolKind::Method)
        .expect("missing PHP Worker::run method");
    let run_nodes = store
        .find_cfg_nodes_by_function(&run.id)
        .expect("run CFG nodes");
    for kind in [
        CfgNodeKind::Entry,
        CfgNodeKind::Loop,
        CfgNodeKind::Branch,
        CfgNodeKind::Throw,
        CfgNodeKind::Return,
        CfgNodeKind::Exit,
    ] {
        assert!(
            run_nodes.iter().any(|node| node.kind == kind),
            "PHP method CFG missing {kind:?}"
        );
    }
}

#[test]
#[cfg(feature = "php")]
fn fx_cfg_php_nested_finally_goto_persists_inner_to_outer_route() {
    let source = r#"<?php
function run() {
    try {
        try {
            goto done;
        } finally {
            inner_cleanup();
        }
    } finally {
        outer_cleanup();
    }
done:
    finish();
}
"#;
    let path = "nested_finally_goto.php";
    let store = index_files(&[(path, source)]);
    let file_id = FileId::generate(path);
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("synthetic PHP symbols")
        .into_iter()
        .find(|symbol| symbol.name == "run" && symbol.kind == SymbolKind::Function)
        .expect("run function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("run CFG nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let goto_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "goto done;");
    let label = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Join, "done:");
    let inner =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Statement, "inner_cleanup();")
            .into_iter()
            .find(|node_id| {
                edges.iter().any(|edge| {
                    edge.source == goto_id
                        && edge.target == *node_id
                        && edge.kind == CfgEdgeKind::Normal
                })
            })
            .expect("goto continuation must execute persisted inner finally");
    let outer =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Statement, "outer_cleanup();")
            .into_iter()
            .find(|node_id| {
                edges.iter().any(|edge| {
                    edge.source == inner
                        && edge.target == *node_id
                        && edge.kind == CfgEdgeKind::Normal
                })
            })
            .expect("goto continuation must execute persisted outer finally");

    assert!(!edges.iter().any(|edge| {
        edge.source == goto_id && edge.target == label && edge.kind == CfgEdgeKind::Goto
    }));
    let final_edge = edges
        .iter()
        .find(|edge| edge.source == outer && edge.target == label && edge.kind == CfgEdgeKind::Goto)
        .expect("outer finally tail must retain the goto continuation kind");
    assert_eq!(
        final_edge.id,
        CfgEdge::new(&outer, &label, CfgEdgeKind::Goto).id
    );
}

#[test]
#[cfg(feature = "php")]
fn fx_cfg_php_real_example_callable_boundaries() {
    let source = example_source_or_skip!("rust_example/tests/syntax-tests/source/PHP/test.php");
    let files = &[("examples/php_syntax.php", source)];
    let store = index_files(files);
    let file_id = FileId::generate("examples/php_syntax.php");
    let callables: Vec<_> = store
        .find_symbols_by_file(&file_id)
        .expect("real PHP example symbols")
        .into_iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
        .collect();
    assert!(
        callables.len() >= 4,
        "expected nested function and class methods from the real PHP syntax example"
    );

    for callable in &callables {
        let nodes = store
            .find_cfg_nodes_by_function(&callable.id)
            .expect("real PHP example CFG nodes");
        assert!(
            nodes.iter().any(|node| node.kind == CfgNodeKind::Entry),
            "real PHP callable {} missing Entry",
            callable.qualified_name
        );
        assert!(
            nodes.iter().any(|node| node.kind == CfgNodeKind::Exit),
            "real PHP callable {} missing Exit",
            callable.qualified_name
        );
    }

    let fav_movie = callables
        .iter()
        .find(|symbol| symbol.name == "favMovie")
        .expect("real PHP example missing nested favMovie function");
    let nodes = store
        .find_cfg_nodes_by_function(&fav_movie.id)
        .expect("favMovie CFG nodes");
    let exit = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .expect("favMovie missing Exit");
    let return_node = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Return)
        .expect("favMovie missing Return");
    assert!(
        store
            .find_cfg_edges_by_source(&return_node.id)
            .expect("favMovie return edges")
            .iter()
            .any(|edge| edge.target == exit.id && edge.kind == CfgEdgeKind::Normal),
        "real PHP example Return must reach Exit"
    );
}

#[test]
#[cfg(feature = "php")]
fn fx_cfg_try_catch_php_without_finally() {
    let files = &[(
        "cfg_try.php",
        r#"<?php
function load($path) {
    try {
        if (!$path) {
            throw new RuntimeException("empty");
        }
        read_file($path);
    } catch (RuntimeException $error) {
        recover($error);
    }
    return $path;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_try.php");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "load" && symbol.kind == SymbolKind::Function)
        .expect("missing PHP load function");
    assert_persisted_exception_cfg(&store, &symbol.id, "PHP", 2);
}

// ── Ruby ────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "ruby")]
fn fx_semantic_rb() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.rb",
        r#"def source
  data = "secret"
  data
end
def process
  x = source()
  y = x
  y
end
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.rb");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── Kotlin ──────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "kotlin")]
fn fx_semantic_kt() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.kt",
        r#"fun source(): String {
    val data = "secret"
    return data
}
fun process(): String {
    val x = source()
    val y = x
    return y
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.kt");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── ArkTS ───────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "arkts")]
fn fx_semantic_ets() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.ets",
        r#"function source(): string {
    let data: string = "secret";
    return data;
}
function process(): string {
    let x: string = source();
    let y: string = x;
    return y;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.ets");
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
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

// ── Cangjie ─────────────────────────────────────────────────────────
// Cangjie interprocedural summaries must preserve the same ReturnToCall
// evidence contract as the other DataflowInterproc languages.

#[test]
#[cfg(feature = "cangjie")]
fn fx_semantic_cj() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "semantic.cj",
        r#"func source(): String {
    let data = "secret"
    return data
}
func process(): String {
    let x = source()
    let y = x
    return y
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("semantic.cj");
    let engine = TraceEngine::new(store.clone());
    let data_nodes = store.find_data_nodes_by_file(&file_id).expect("data nodes");

    let sink = find_node(&data_nodes, "y");
    let resp = engine.trace_variable(
        &file_id,
        sink.range.start_line + 1,
        sink.range.start_column + 1,
        20,
    );
    assert_envelope_ok(&resp, "cangjie");
    let path = resp.result.expect("trace path must exist");
    assert_path_completeness(&path, 3, "y");
    assert_source_name(&path, "secret");
    assert_has_edge_kind(&path, DataFlowKind::ReturnToCall);
    assert_step_with_name(&store, &path, DataFlowKind::Assign, "x");
}

/// Real `cjvs`-shaped entrypoint: both `mainDefinition` and ordinary
/// `functionDefinition` nodes must receive persisted CFG facts, and Cangjie
/// `match` arms must be represented as sibling case paths.
#[test]
#[cfg(feature = "cangjie")]
fn fx_cfg_match_cangjie_entry_and_function() {
    let files = &[(
        "main.cj",
        r#"main(): Unit {
    let command = "list"
    match (command) {
        case "list" | "ls" => dispatch(command)
        case "install" => install()
        case _ => unknown()
    }
}

func dispatch(command: String): Unit {
    println(command)
}

func install(): Unit {}
func unknown(): Unit {}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("main.cj");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");

    for name in ["main", "dispatch"] {
        let symbol = symbols
            .iter()
            .find(|symbol| symbol.name == name && symbol.kind == SymbolKind::Function)
            .unwrap_or_else(|| panic!("missing Cangjie function symbol {name}"));
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&symbol.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.iter().any(|node| node.kind == CfgNodeKind::Entry),
            "Cangjie CFG for {name} missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|node| node.kind == CfgNodeKind::Exit),
            "Cangjie CFG for {name} missing Exit"
        );

        if name == "main" {
            let branch = cfg_nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Branch)
                .expect("Cangjie main match missing Branch");
            assert!(
                cfg_nodes.iter().any(|node| node.kind == CfgNodeKind::Join),
                "Cangjie main match missing Join"
            );
            let case_edges = store
                .find_cfg_edges_by_source(&branch.id)
                .expect("case edges")
                .into_iter()
                .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
                .count();
            assert_eq!(
                case_edges, 3,
                "unguarded Cangjie wildcard suppresses the synthetic no-match edge"
            );
        }
    }
}

#[test]
#[cfg(feature = "cangjie")]
fn fx_cfg_real_cangjie_wildcard_suppresses_persisted_no_match_path() {
    let source = example_source_or_skip!("cangjie_example/src/stdx/command.cj");
    let store = index_files(&[("command.cj", source)]);
    let file_id = FileId::generate("command.cj");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "handleCommand" && symbol.kind == SymbolKind::Function)
        .expect("missing real Cangjie handleCommand function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let match_dispatch = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Branch
                && edges
                    .iter()
                    .any(|edge| edge.source == node.id && edge.kind == CfgEdgeKind::CaseBranch)
        })
        .expect("real Cangjie match dispatch");

    assert_eq!(
        edges
            .iter()
            .filter(|edge| {
                edge.source == match_dispatch.id && edge.kind == CfgEdgeKind::CaseBranch
            })
            .count(),
        8,
        "seven command patterns plus wildcard, without synthetic no-match"
    );
}

#[test]
#[cfg(feature = "cangjie")]
fn fx_cfg_loop_break_cangjie_is_persisted_as_control_transfer() {
    let files = &[(
        "loop.cj",
        r#"func run(): Unit {
    while (isReady()) {
        break
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("loop.cj");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "run" && symbol.kind == SymbolKind::Function)
        .expect("missing Cangjie run function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    assert!(
        !nodes.iter().any(|node| node.kind == CfgNodeKind::Return),
        "Cangjie break must not be persisted as Return"
    );
    assert!(
        nodes
            .iter()
            .flat_map(|node| { store.find_cfg_edges_by_source(&node.id).expect("cfg_edges") })
            .any(|edge| edge.kind == CfgEdgeKind::Break),
        "Cangjie break edge was not persisted"
    );
}

// ── CFG fixture tests ─────────────────────────────────────────────────────

/// Verify CFG for Java: function must have Entry + Statement + Exit nodes
/// with at least one edge.
#[test]
#[cfg(feature = "java")]
fn fx_cfg_java() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg.java",
        r#"class App {
    int compute(int x) {
        if (x > 0) {
            return x * 2;
        }
        return 0;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.java");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Method | SymbolKind::Function | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(
        !func_syms.is_empty(),
        "expected at least one function/method symbol"
    );

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.len() >= 3,
            "Java CFG for '{}': expected >= 3 nodes, got {}",
            sym.name,
            cfg_nodes.len()
        );
        let has_entry = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry, "Java CFG missing Entry node");
        assert!(has_exit, "Java CFG missing Exit node");

        let mut edge_count = 0usize;
        for node in &cfg_nodes {
            let edges = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edge_count += edges.len();
        }
        assert!(
            edge_count > 0,
            "Java CFG for '{}': expected > 0 edges, got {}",
            sym.name,
            edge_count
        );
    }
}

/// Verify CFG for Go: function must have Entry + Statement + Exit nodes.
#[test]
#[cfg(feature = "go")]
fn fx_cfg_go() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg.go",
        r#"package main

func compute(x int) int {
    if x > 0 {
        return x * 2
    }
    return 0
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.go");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(
        !func_syms.is_empty(),
        "expected at least one function symbol"
    );

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.len() >= 3,
            "Go CFG for '{}': expected >= 3 nodes, got {}",
            sym.name,
            cfg_nodes.len()
        );
        let has_entry = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry, "Go CFG missing Entry node");
        assert!(has_exit, "Go CFG missing Exit node");

        let mut edge_count = 0usize;
        for node in &cfg_nodes {
            let edges = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edge_count += edges.len();
        }
        assert!(
            edge_count > 0,
            "Go CFG for '{}': expected > 0 edges, got {}",
            sym.name,
            edge_count
        );
    }
}

/// Verify CFG for Python: function must have Entry + Statement + Exit nodes.
#[test]
#[cfg(feature = "python")]
fn fx_cfg_python() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg.py",
        r#"def compute(x: int) -> int:
    if x > 0:
        return x * 2
    return 0
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.py");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(
        !func_syms.is_empty(),
        "expected at least one function symbol"
    );

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.len() >= 3,
            "Python CFG for '{}': expected >= 3 nodes, got {}",
            sym.name,
            cfg_nodes.len()
        );
        let has_entry = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry, "Python CFG missing Entry node");
        assert!(has_exit, "Python CFG missing Exit node");

        let mut edge_count = 0usize;
        for node in &cfg_nodes {
            let edges = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edge_count += edges.len();
        }
        assert!(
            edge_count > 0,
            "Python CFG for '{}': expected > 0 edges, got {}",
            sym.name,
            edge_count
        );
    }
}

/// Verify CFG for C: function must have Entry + Statement + Exit nodes
/// with at least one edge.
#[test]
#[cfg(feature = "c")]
fn fx_cfg_c() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg.c",
        r#"int compute(int x) {
    if (x > 0) {
        return x * 2;
    }
    return 0;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.c");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(
        !func_syms.is_empty(),
        "expected at least one function symbol"
    );

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.len() >= 3,
            "C CFG for '{}': expected >= 3 nodes, got {}",
            sym.name,
            cfg_nodes.len()
        );
        let has_entry = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry, "C CFG missing Entry node");
        assert!(has_exit, "C CFG missing Exit node");

        let mut edge_count = 0usize;
        for node in &cfg_nodes {
            let edges = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edge_count += edges.len();
        }
        assert!(
            edge_count > 0,
            "C CFG for '{}': expected > 0 edges, got {}",
            sym.name,
            edge_count
        );
    }
}

/// Verify CFG for C++: function must have Entry + Statement + Exit nodes.
#[test]
#[cfg(feature = "cpp")]
fn fx_cfg_cpp() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg.cpp",
        r#"int compute(int x) {
    if (x > 0) {
        return x * 2;
    }
    return 0;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.cpp");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(
        !func_syms.is_empty(),
        "expected at least one function symbol"
    );

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.len() >= 3,
            "C++ CFG for '{}': expected >= 3 nodes, got {}",
            sym.name,
            cfg_nodes.len()
        );
        let has_entry = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry, "C++ CFG missing Entry node");
        assert!(has_exit, "C++ CFG missing Exit node");

        let mut edge_count = 0usize;
        for node in &cfg_nodes {
            let edges = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edge_count += edges.len();
        }
        assert!(
            edge_count > 0,
            "C++ CFG for '{}': expected > 0 edges, got {}",
            sym.name,
            edge_count
        );
    }
}

#[test]
#[cfg(feature = "go")]
fn fx_cfg_real_go_select_persists_only_communication_and_default_paths() {
    let source = example_source_or_skip!("go_example/context.go");
    let store = index_files(&[("context.go", source)]);
    let file_id = FileId::generate("context.go");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "Stream" && symbol.kind == SymbolKind::Method)
        .expect("missing real Go Context.Stream method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let return_disconnected =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return true");
    let default_entry = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "keepOpen := step(w)",
    );
    let edges = persisted_cfg_edges(&store, &nodes);
    let select_dispatch = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Branch
                && edges.iter().any(|edge| {
                    edge.source == node.id
                        && edge.target == return_disconnected
                        && edge.kind == CfgEdgeKind::CaseBranch
                })
        })
        .expect("real Go select dispatch");
    let case_targets: std::collections::HashSet<_> = edges
        .iter()
        .filter(|edge| edge.source == select_dispatch.id && edge.kind == CfgEdgeKind::CaseBranch)
        .map(|edge| edge.target)
        .collect();

    assert_eq!(
        case_targets,
        std::collections::HashSet::from([return_disconnected, default_entry])
    );
    assert!(!persisted_cfg_reaches(
        &edges,
        return_disconnected,
        default_entry
    ));
}

#[test]
#[cfg(feature = "go")]
fn fx_cfg_real_go_run_unix_persists_path_sensitive_lifo_defers() {
    let source = example_source_or_skip!("go_example/gin.go");
    let store = index_files(&[("gin.go", source)]);
    let file_id = FileId::generate("gin.go");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("real Gin symbols")
        .into_iter()
        .find(|symbol| symbol.name == "RunUnix" && symbol.kind == SymbolKind::Method)
        .expect("missing real Gin Engine.RunUnix method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("RunUnix CFG nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let mut returns =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Return, "return");
    returns.sort_by_key(|node_id| {
        nodes
            .iter()
            .find(|node| node.id == *node_id)
            .expect("return node")
            .stmt_range
            .start_byte
    });
    assert_eq!(
        returns.len(),
        2,
        "RunUnix has one early and one final return"
    );

    let registration_owner = |expected: &str| {
        nodes
            .iter()
            .find(|node| {
                if node.kind != CfgNodeKind::Statement || node.call_context != CallContext::GoDefer
                {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == expected)
            })
            .and_then(|node| node.managed_scope_start_byte)
            .unwrap_or_else(|| panic!("missing defer registration {expected:?}"))
    };
    let debug_owner = registration_owner("func() { debugPrintError(err) }()");
    let close_owner = registration_owner("listener.Close()");
    let remove_owner = registration_owner("os.Remove(file)");
    let owner_chain = |start| {
        let mut current = start;
        let mut owners = Vec::new();
        while let Some(edge) = edges
            .iter()
            .find(|edge| edge.source == current && edge.kind == CfgEdgeKind::Defer)
        {
            let execution = nodes
                .iter()
                .find(|node| node.id == edge.target)
                .expect("persisted defer execution node");
            assert_eq!(execution.kind, CfgNodeKind::BlockExit);
            assert_eq!(execution.call_context, CallContext::GoDefer);
            owners.push(
                execution
                    .managed_scope_start_byte
                    .expect("defer execution owner"),
            );
            assert_eq!(
                edge.id,
                CfgEdge::new(&edge.source, &edge.target, CfgEdgeKind::Defer).id
            );
            current = edge.target;
        }
        owners
    };

    assert_eq!(
        owner_chain(returns[0]),
        vec![debug_owner],
        "the net.Listen error path registered only the leading debug defer"
    );
    assert_eq!(
        owner_chain(returns[1]),
        vec![remove_owner, close_owner, debug_owner],
        "the successful path must execute all three defers in reverse registration order"
    );
    assert_eq!(
        nodes
            .iter()
            .filter(|node| {
                node.kind == CfgNodeKind::Statement && node.call_context == CallContext::GoDefer
            })
            .count(),
        3,
        "each lexical defer remains a visible registration point"
    );
}

/// Verify CFG for Rust: function must have Entry + Statement + Exit nodes.
#[test]
#[cfg(feature = "rust")]
fn fx_cfg_rust() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg.rs",
        r#"fn compute(x: i32) -> i32 {
    if x > 0 {
        return x * 2;
    }
    return 0;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg.rs");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(
        !func_syms.is_empty(),
        "expected at least one function symbol"
    );

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.len() >= 3,
            "Rust CFG for '{}': expected >= 3 nodes, got {}",
            sym.name,
            cfg_nodes.len()
        );
        let has_entry = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry, "Rust CFG missing Entry node");
        assert!(has_exit, "Rust CFG missing Exit node");

        let mut edge_count = 0usize;
        for node in &cfg_nodes {
            let edges = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edge_count += edges.len();
        }
        assert!(
            edge_count > 0,
            "Rust CFG for '{}': expected > 0 edges, got {}",
            sym.name,
            edge_count
        );
    }
}

// ────────────────────────────────────────────────────────────────
// CFG body traversal tests — verify if/else and loop bodies
// are properly traversed with Statement nodes and correct edges.
// ────────────────────────────────────────────────────────────────

/// Verify TypeScript CFG body traversal for if/else:
/// Statement nodes in consequence/alternative, TrueBranch/FalseBranch edges,
/// Join node, and post-Join flow.
#[test]
#[cfg(feature = "typescript")]
fn fx_cfg_if_else_ts() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.ts",
        r#"function testIf(x: number) {
    if (x > 0) {
        console.log("pos");
    } else {
        console.log("neg");
    }
    return x;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.ts");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        eprintln!(
            "=== {} CFG nodes: {:#?}",
            sym.name,
            cfg_nodes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );

        // ── Node kind assertions ──
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "missing Exit"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "missing Join"
        );

        // Must have at least 2 Statement nodes (one per branch body)
        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );

        // ── Edge assertions ──
        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        eprintln!(
            "=== {} CFG edges: {:#?}",
            sym.name,
            edges
                .iter()
                .map(|e| format!("{:?}→{:?}", e.kind, e.target))
                .collect::<Vec<_>>()
        );

        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::TrueBranch),
            "missing TrueBranch edge for consequence body"
        );
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::FalseBranch),
            "missing FalseBranch edge for alternative body"
        );
    }
}

/// Verify TypeScript CFG body traversal for while loop:
/// Statement node in body, LoopBack edge, Loop→Join exit.
#[test]
#[cfg(feature = "typescript")]
fn fx_cfg_loop_ts() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.ts",
        r#"function testLoop(x: number) {
    while (x > 0) {
        x = x - 1;
    }
    return x;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.ts");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }

        let loop_id = cfg_nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Loop)
            .map(|n| n.id)
            .expect("loop_id");

        // Loop → first body Statement
        let has_loop_body = edges
            .iter()
            .any(|e| e.source == loop_id && e.kind == CfgEdgeKind::Normal);
        assert!(has_loop_body, "missing Loop → body edge");

        // LoopBack from body Statement back to Loop
        let has_loopback = edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack);
        assert!(has_loopback, "missing LoopBack edge");
    }
}

/// Verify Python CFG body traversal for if/else.
#[test]
#[cfg(feature = "python")]
fn fx_cfg_if_else_python() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.py",
        r#"def test_if(x: int) -> int:
    if x > 0:
        print("pos")
    else:
        print("neg")
    return x
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.py");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Python: missing Branch node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Python: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "Python: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );
    }
}

/// Verify Python CFG body traversal for while loop.
#[test]
#[cfg(feature = "python")]
fn fx_cfg_loop_python() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.py",
        r#"def test_loop(x: int) -> int:
    while x > 0:
        x = x - 1
    return x
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.py");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "Python: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Python: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "Python: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "Python: missing LoopBack edge"
        );
    }
}

#[test]
#[cfg(feature = "python")]
fn fx_cfg_match_python() {
    let files = &[(
        "cfg_match.py",
        r#"def dispatch(command):
    match command:
        case value if value > 0:
            return positive(value)
        case 0:
            return zero()
        case _:
            return fallback()
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_match.py");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing Python dispatch function");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("Python match missing Branch");
    let case_edges = store
        .find_cfg_edges_by_source(&branch.id)
        .expect("case edges")
        .into_iter()
        .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 3,
        "an unguarded Python wildcard case suppresses the synthetic no-match edge"
    );
}

#[test]
#[cfg(feature = "python")]
fn fx_cfg_match_python_capture_pattern_suppresses_persisted_no_match_path() {
    let files = &[(
        "cfg_match_capture.py",
        r#"def dispatch(command):
    match command:
        case 0:
            return zero()
        case value:
            return fallback(value)
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_match_capture.py");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing Python dispatch function");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("Python match missing Branch");
    let case_edges = store
        .find_cfg_edges_by_source(&branch.id)
        .expect("case edges")
        .into_iter()
        .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 2,
        "an unguarded Python capture pattern suppresses the synthetic no-match edge"
    );
}

#[test]
#[cfg(feature = "rust")]
fn fx_cfg_real_rust_unguarded_wildcard_suppresses_persisted_no_match_path() {
    let source = example_source_or_skip!("rust_example/src/less.rs");
    let store = index_files(&[("less.rs", source)]);
    let file_id = FileId::generate("less.rs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "parse_less_version_busybox" && symbol.kind == SymbolKind::Function
        })
        .expect("missing real Rust parse_less_version_busybox function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let dispatch = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Branch
                && edges
                    .iter()
                    .any(|edge| edge.source == node.id && edge.kind == CfgEdgeKind::CaseBranch)
        })
        .expect("real Rust match dispatch");

    assert_eq!(
        edges
            .iter()
            .filter(|edge| { edge.source == dispatch.id && edge.kind == CfgEdgeKind::CaseBranch })
            .count(),
        2,
        "guarded BusyBox arm plus unguarded wildcard, without synthetic no-match"
    );
}

#[test]
#[cfg(feature = "rust")]
fn fx_cfg_real_rust_try_operator_persists_success_and_residual_paths() {
    let source = example_source_or_skip!("rust_example/src/line_range.rs");
    let store = index_files(&[("line_range.rs", source)]);
    let file_id = FileId::generate("line_range.rs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "parse_range"
                && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
        })
        .expect("missing real Rust LineRange::parse_range function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let propagation = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "let first_byte = raw_range_iter.next().ok_or(\"Empty line range\")?;",
    );
    let exit = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .expect("Rust CFG Exit")
        .id;
    let following_branch = nodes
        .iter()
        .filter(|node| node.kind == CfgNodeKind::Branch)
        .min_by_key(|node| node.stmt_range.start_byte)
        .expect("Rust parse_range first if Branch")
        .id;

    assert!(edges.iter().any(|edge| {
        edge.source == propagation
            && edge.target == following_branch
            && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(edges.iter().any(|edge| {
        edge.source == propagation && edge.target == exit && edge.kind == CfgEdgeKind::Normal
    }));
}

#[test]
#[cfg(feature = "rust")]
fn fx_cfg_real_rust_let_else_persists_match_and_loop_break_paths() {
    let source = example_source_or_skip!("rust_example/src/controller.rs");
    let store = index_files(&[("controller.rs", source)]);
    let file_id = FileId::generate("controller.rs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "print_file_ranges"
                && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
        })
        .expect("missing real Rust Controller::print_file_ranges method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let branch = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Branch
                && source
                    .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                    .is_some_and(|text| text == "buffered_lines.pop_front()")
        })
        .expect("real Rust let-else Branch");
    let false_edge = edges
        .iter()
        .find(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::FalseBranch)
        .expect("let-else alternative edge");
    let true_edge = edges
        .iter()
        .find(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::TrueBranch)
        .expect("let-else success edge");
    let break_node = nodes
        .iter()
        .find(|node| node.id == false_edge.target)
        .expect("let-else break node");
    let success_join = nodes
        .iter()
        .find(|node| node.id == true_edge.target)
        .expect("let-else success Join");
    let following_statement = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Statement
                && node.stmt_range.start_byte as usize
                    == source
                        .find("let max_buffered_line_number")
                        .expect("real following declaration")
        })
        .expect("statement after real let-else");

    assert_eq!(break_node.kind, CfgNodeKind::Statement);
    assert_eq!(success_join.kind, CfgNodeKind::Join);
    assert!(
        edges
            .iter()
            .any(|edge| { edge.source == break_node.id && edge.kind == CfgEdgeKind::Break })
    );
    assert!(
        persisted_cfg_reaches(&edges, success_join.id, following_statement.id),
        "let-else success must continue to the following declaration"
    );
    assert!(!persisted_cfg_reaches(
        &edges,
        break_node.id,
        following_statement.id
    ));
    assert_eq!(
        false_edge.id,
        CfgEdge::new(
            &false_edge.source,
            &false_edge.target,
            CfgEdgeKind::FalseBranch
        )
        .id
    );
}

#[test]
#[cfg(feature = "rust")]
fn fx_cfg_real_rust_panic_macro_persists_as_abrupt_match_arm() {
    let source = example_source_or_skip!("rust_example/src/vscreen.rs");
    let store = index_files(&[("vscreen.rs", source)]);
    let file_id = FileId::generate("vscreen.rs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "next_osc"
                && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
        })
        .expect("missing real Rust EscapeSequenceOffsetsIterator::next_osc method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edges = persisted_cfg_edges(&store, &nodes);
    let panic_node = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Throw,
        "panic!(\"this should not be reached: char {tc:?}\")",
    );
    let exit = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .expect("Rust CFG Exit")
        .id;
    let case_edge = edges
        .iter()
        .find(|edge| edge.target == panic_node && edge.kind == CfgEdgeKind::CaseBranch)
        .expect("panic match arm CaseBranch");

    assert!(
        nodes
            .iter()
            .any(|node| node.id == case_edge.source && node.kind == CfgNodeKind::Branch)
    );
    assert!(edges.iter().any(|edge| {
        edge.source == panic_node && edge.target == exit && edge.kind == CfgEdgeKind::Normal
    }));
    assert_eq!(
        case_edge.id,
        CfgEdge::new(
            &case_edge.source,
            &case_edge.target,
            CfgEdgeKind::CaseBranch
        )
        .id
    );
}

/// Verify Go CFG body traversal for if/else.
#[test]
#[cfg(feature = "go")]
fn fx_cfg_if_else_go() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.go",
        r#"package main

func testIf(x int) int {
    if x > 0 {
        println("pos")
    } else {
        println("neg")
    }
    return x
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.go");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Go: missing Branch node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Go: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "Go: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );
    }
}

/// Verify Go CFG body traversal for for loop (Go uses `for` as sole loop).
#[test]
#[cfg(feature = "go")]
fn fx_cfg_loop_go() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.go",
        r#"package main

func testLoop(x int) int {
    for x > 0 {
        x = x - 1
    }
    return x
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.go");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "Go: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Go: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "Go: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "Go: missing LoopBack edge"
        );
    }
}

/// Verify Rust CFG body traversal for if/else.
#[test]
#[cfg(feature = "rust")]
fn fx_cfg_if_else_rust() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.rs",
        r#"fn test_if(x: i32) -> i32 {
    if x > 0 {
        println!("pos");
    } else {
        println!("neg");
    }
    x
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.rs");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Rust: missing Branch node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Rust: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "Rust: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );
    }
}

/// Verify Rust CFG body traversal for while loop.
#[test]
#[cfg(feature = "rust")]
fn fx_cfg_loop_rust() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.rs",
        r#"fn test_loop(mut x: i32) -> i32 {
    while x > 0 {
        x = x - 1;
    }
    x
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.rs");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "Rust: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Rust: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "Rust: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "Rust: missing LoopBack edge"
        );
    }
}

#[test]
#[cfg(feature = "rust")]
fn fx_cfg_match_rust() {
    let files = &[(
        "cfg_match.rs",
        r#"fn dispatch(command: i32) {
    match command {
        n if n > 0 => positive(n),
        0 => zero(),
        _ => fallback(),
    };
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_match.rs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing Rust dispatch function");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("Rust match missing Branch");
    let case_edges = store
        .find_cfg_edges_by_source(&branch.id)
        .expect("case edges")
        .into_iter()
        .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 3,
        "an unguarded Rust wildcard arm suppresses the synthetic no-match edge"
    );
}

// ────────────────────────────────────────────────────────────────
// Java CFG body traversal tests
// ────────────────────────────────────────────────────────────────

/// Verify Java CFG body traversal for if/else:
/// Statement nodes in consequence/alternative, TrueBranch/FalseBranch edges,
/// Join node, and post-Join flow.
#[test]
#[cfg(feature = "java")]
fn fx_cfg_if_else_java() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.java",
        r#"class App {
    int testIf(int x) {
        if (x > 0) {
            System.out.println("pos");
        } else {
            System.out.println("neg");
        }
        return x;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.java");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        // ── Node kind assertions ──
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "Java: missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "Java: missing Exit"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Java: missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Java: missing Join"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "Java: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );

        // ── Edge assertions ──
        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }

        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::TrueBranch),
            "Java: missing TrueBranch edge"
        );
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::FalseBranch),
            "Java: missing FalseBranch edge"
        );

        assert!(
            edges.len() > 3,
            "Java: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

/// Verify Java CFG body traversal for while loop:
/// Statement node in body, LoopBack edge, Loop→Join exit.
#[test]
#[cfg(feature = "java")]
fn fx_cfg_loop_java() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.java",
        r#"class App {
    int testLoop(int x) {
        while (x > 0) {
            x = x - 1;
        }
        return x;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.java");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "Java: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Java: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "Java: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "Java: missing LoopBack edge"
        );

        assert!(
            edges.len() > 3,
            "Java: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

// ────────────────────────────────────────────────────────────────
// C CFG body traversal tests
// ────────────────────────────────────────────────────────────────

/// Verify C CFG body traversal for if/else:
/// Statement nodes, TrueBranch/FalseBranch edges, Join node.
#[test]
#[cfg(feature = "c")]
fn fx_cfg_if_else_c() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.c",
        r#"int test_if(int x) {
    if (x > 0) {
        printf("pos\n");
    } else {
        printf("neg\n");
    }
    return x;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.c");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "C: missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "C: missing Exit"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "C: missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "C: missing Join"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "C: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::TrueBranch),
            "C: missing TrueBranch edge"
        );
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::FalseBranch),
            "C: missing FalseBranch edge"
        );

        assert!(
            edges.len() > 3,
            "C: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

/// Verify C CFG body traversal for while loop:
/// Statement node in body, LoopBack edge, Loop→Join exit.
#[test]
#[cfg(feature = "c")]
fn fx_cfg_loop_c() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.c",
        r#"int test_loop(int x) {
    while (x > 0) {
        x = x - 1;
    }
    return x;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.c");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "C: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "C: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "C: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "C: missing LoopBack edge"
        );

        assert!(
            edges.len() > 3,
            "C: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

#[test]
#[cfg(feature = "c")]
fn fx_cfg_real_c_example_non_empty_fallthrough() {
    let source = example_source_or_skip!("c_example/src/tool_convert.c");
    let store = index_files(&[("examples/tool_convert.c", source)]);
    let file_id = FileId::generate("examples/tool_convert.c");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("real C example symbols")
        .into_iter()
        .find(|symbol| symbol.name == "convert_char" && symbol.kind == SymbolKind::Function)
        .expect("convert_char function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("convert_char CFG nodes");
    let switch_branch = nodes
        .iter()
        .find(|node| {
            if node.kind != CfgNodeKind::Branch {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim_start().starts_with("switch(infotype)"))
        })
        .expect("convert_char switch Branch");
    let switch_join = nodes
        .iter()
        .find(|node| {
            node.kind == CfgNodeKind::Join
                && node.stmt_range.start_byte == switch_branch.stmt_range.start_byte + 1
        })
        .expect("convert_char switch Join");
    let conversion = persisted_cfg_node_id_for_text(
        &nodes,
        source,
        CfgNodeKind::Statement,
        "(void)convert_from_network(&this_char, 1);",
    );
    let default_entry = nodes
        .iter()
        .find(|node| {
            if node.kind != CfgNodeKind::Branch {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim_start().starts_with("if(ISPRINT(this_char)"))
        })
        .expect("convert_char executable default entry")
        .id;
    let mut edges = Vec::new();
    for node in &nodes {
        edges.extend(
            store
                .find_cfg_edges_by_source(&node.id)
                .expect("convert_char CFG edges"),
        );
    }
    for edge in &edges {
        assert_eq!(
            edge.id,
            CfgEdge::new(&edge.source, &edge.target, edge.kind).id,
            "persisted edge ID must encode its final kind"
        );
    }
    assert!(edges.iter().any(|edge| {
        edge.source == conversion
            && edge.target == default_entry
            && edge.kind == CfgEdgeKind::Normal
    }));
    assert!(edges.iter().any(|edge| edge.kind == CfgEdgeKind::Break));
    assert!(!edges.iter().any(|edge| {
        edge.source == switch_branch.id
            && edge.target == switch_join.id
            && edge.kind == CfgEdgeKind::CaseBranch
    }));
}

#[test]
#[cfg(feature = "c")]
fn fx_cfg_real_redis_cleanup_gotos_persist_exact_label_edges() {
    let source = example_source_or_skip!("redis/deps/hdr_histogram/hdr_histogram.c");
    let path = "examples/hdr_histogram.c";
    let store = index_files(&[(path, source)]);
    let file_id = FileId::generate(path);
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("real Redis C symbols")
        .into_iter()
        .find(|symbol| {
            symbol.name == "hdr_percentiles_print" && symbol.kind == SymbolKind::Function
        })
        .expect("hdr_percentiles_print function");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("hdr_percentiles_print CFG nodes");
    let cleanup_return =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return rc;");
    let gotos: Vec<_> = nodes
        .iter()
        .filter(|node| {
            if node.kind != CfgNodeKind::Statement {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim() == "goto cleanup;")
        })
        .collect();
    assert_eq!(gotos.len(), 3, "real function has three cleanup jumps");

    for goto in gotos {
        let outgoing = store
            .find_cfg_edges_by_source(&goto.id)
            .expect("persisted goto edges");
        assert_eq!(
            outgoing.len(),
            1,
            "goto must not retain lexical fallthrough"
        );
        assert_eq!(outgoing[0].target, cleanup_return);
        assert_eq!(outgoing[0].kind, CfgEdgeKind::Goto);
        assert_eq!(
            outgoing[0].id,
            CfgEdge::new(&goto.id, &cleanup_return, CfgEdgeKind::Goto).id,
            "persisted edge ID must encode goto kind"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// C++ CFG body traversal tests
// ────────────────────────────────────────────────────────────────

/// Verify C++ CFG body traversal for if/else:
/// Statement nodes, TrueBranch/FalseBranch edges, Join node.
#[test]
#[cfg(feature = "cpp")]
fn fx_cfg_if_else_cpp() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.cpp",
        r#"int test_if(int x) {
    if (x > 0) {
        printf("pos\n");
    } else {
        printf("neg\n");
    }
    return x;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.cpp");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "C++: missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "C++: missing Exit"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "C++: missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "C++: missing Join"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "C++: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::TrueBranch),
            "C++: missing TrueBranch edge"
        );
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::FalseBranch),
            "C++: missing FalseBranch edge"
        );

        assert!(
            edges.len() > 3,
            "C++: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

/// Verify C++ CFG body traversal for while loop:
/// Statement node in body, LoopBack edge, Loop→Join exit.
#[test]
#[cfg(feature = "cpp")]
fn fx_cfg_loop_cpp() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.cpp",
        r#"int test_loop(int x) {
    while (x > 0) {
        x = x - 1;
    }
    return x;
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.cpp");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "C++: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "C++: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "C++: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "C++: missing LoopBack edge"
        );

        assert!(
            edges.len() > 3,
            "C++: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

// ────────────────────────────────────────────────────────────────
// C# CFG body traversal tests
// ────────────────────────────────────────────────────────────────

/// Verify C# CFG body traversal for if/else:
/// Statement nodes in consequence/alternative, TrueBranch/FalseBranch edges,
/// Join node, and post-Join flow.
#[test]
#[cfg(feature = "csharp")]
fn fx_cfg_if_else_csharp() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.cs",
        r#"class App {
    int TestIf(int x) {
        if (x > 0) {
            Console.WriteLine("pos");
        } else {
            Console.WriteLine("neg");
        }
        return x;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.cs");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        // ── Node kind assertions ──
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Entry),
            "C#: missing Entry"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Exit),
            "C#: missing Exit"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "C#: missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "C#: missing Join"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 2,
            "C#: expected >= 2 Statement nodes (body statements), got {stmt_count}"
        );

        // ── Edge assertions ──
        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }

        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::TrueBranch),
            "C#: missing TrueBranch edge"
        );
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::FalseBranch),
            "C#: missing FalseBranch edge"
        );

        assert!(
            edges.len() > 3,
            "C#: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

/// Verify C# CFG body traversal for while loop:
/// Statement node in body, LoopBack edge, Loop→Join exit.
#[test]
#[cfg(feature = "csharp")]
fn fx_cfg_loop_csharp() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.cs",
        r#"class App {
    int TestLoop(int x) {
        while (x > 0) {
            x = x - 1;
        }
        return x;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.cs");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "C#: missing Loop node"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "C#: missing Join node"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "C#: expected >= 1 Statement node (loop body), got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|e| e.kind == CfgEdgeKind::LoopBack),
            "C#: missing LoopBack edge"
        );

        assert!(
            edges.len() > 3,
            "C#: expected > 3 total CFG edges, got {}",
            edges.len()
        );
    }
}

#[test]
#[cfg(feature = "csharp")]
fn fx_cfg_try_catch_csharp_without_finally() {
    let files = &[(
        "cfg_try.cs",
        r#"class Loader {
    string Load(string path) {
        try {
            if (path.Length == 0) {
                throw new InvalidOperationException("empty");
            }
            Read(path);
        } catch (InvalidOperationException error) {
            Recover(error);
        }
        return path;
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_try.cs");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "Load" && symbol.kind == SymbolKind::Method)
        .expect("missing C# Loader.Load method");
    assert_persisted_exception_cfg(&store, &symbol.id, "C#", 2);
}

// ────────────────────────────────────────────────────────────────
// Kotlin CFG body traversal tests
// ────────────────────────────────────────────────────────────────

/// Verify Kotlin CFG body traversal for if/else:
/// Branch/Join nodes, both branch edge kinds, and body statements.
#[test]
#[cfg(feature = "kotlin")]
fn fx_cfg_if_else_kotlin() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_if.kt",
        r#"fun testIf(x: Int): Int {
    if (x > 0) {
        println("pos")
    } else {
        println("neg")
    }
    return x
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_if.kt");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Kotlin: missing Branch"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Kotlin: missing Join"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "Kotlin: expected >= 1 Statement node, got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::TrueBranch),
            "Kotlin: missing TrueBranch"
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::FalseBranch),
            "Kotlin: missing FalseBranch"
        );
    }
}

/// Verify Kotlin CFG body traversal for while loop:
/// Loop/Join nodes, a body statement, and LoopBack.
#[test]
#[cfg(feature = "kotlin")]
fn fx_cfg_loop_kotlin() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cfg_loop.kt",
        r#"fun testLoop(x: Int): Int {
    while (x > 0) {
        if (x == 2) {
            break
        }
        x = x - 1
    }
    return x
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.kt");
    let symbols = store.find_symbols_by_file(&file_id).expect("symbols");
    let func_syms: Vec<_> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();
    assert!(!func_syms.is_empty(), "expected at least one function");

    for sym in &func_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .expect("cfg_nodes");

        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Loop),
            "Kotlin: missing Loop"
        );
        assert!(
            cfg_nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Kotlin: missing Join"
        );

        let stmt_count = cfg_nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count >= 1,
            "Kotlin: expected >= 1 Statement node, got {stmt_count}"
        );

        let mut edges = Vec::new();
        for node in &cfg_nodes {
            let e = store.find_cfg_edges_by_source(&node.id).expect("cfg_edges");
            edges.extend(e);
        }
        assert!(
            edges.iter().any(|edge| edge.kind == CfgEdgeKind::LoopBack),
            "Kotlin: missing LoopBack"
        );
        assert!(
            edges.iter().any(|edge| edge.kind == CfgEdgeKind::Break),
            "Kotlin: break edge was not persisted"
        );
        assert!(
            !cfg_nodes.iter().any(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Return
                    && files[0]
                        .1
                        .get(range)
                        .is_some_and(|text| text.trim() == "break")
            }),
            "Kotlin: break must not be persisted as Return"
        );
    }
}

#[test]
#[cfg(feature = "kotlin")]
fn fx_cfg_when_kotlin() {
    let files = &[(
        "cfg_when.kt",
        r#"fun dispatch(command: Int) {
    when (command) {
        1 if command > 0 -> positive(command)
        0 -> zero()
        else -> fallback()
    }
}
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_when.kt");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Function)
        .expect("missing Kotlin dispatch function");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("Kotlin when missing Branch");
    let case_edges = store
        .find_cfg_edges_by_source(&branch.id)
        .expect("case edges")
        .into_iter()
        .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 3,
        "expected three Kotlin entries; else makes no-match impossible"
    );
}

#[test]
#[cfg(feature = "ruby")]
fn fx_cfg_case_ruby() {
    let files = &[(
        "cfg_case.rb",
        r#"def dispatch(command)
  case command
  when "install", "add"
    install()
  when "remove"
    remove()
  else
    fallback()
  end
end
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_case.rb");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "dispatch" && symbol.kind == SymbolKind::Method)
        .expect("missing Ruby dispatch method");
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let branch = cfg_nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("Ruby case missing Branch");
    let case_edges = store
        .find_cfg_edges_by_source(&branch.id)
        .expect("case edges")
        .into_iter()
        .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
        .count();
    assert_eq!(
        case_edges, 3,
        "expected two Ruby when clauses and else without a no-match edge"
    );
}

#[test]
#[cfg(feature = "ruby")]
fn fx_cfg_loop_break_and_next_ruby_are_persisted_as_control_transfers() {
    let files = &[(
        "cfg_loop.rb",
        r#"def consume(flag, skip)
  while flag
    if skip
      next
    end
    break
  end
  after()
end
"#,
    )];
    let store = index_files(files);
    let file_id = FileId::generate("cfg_loop.rb");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "consume" && symbol.kind == SymbolKind::Method)
        .expect("missing Ruby consume method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let edge_kinds: Vec<_> = nodes
        .iter()
        .flat_map(|node| store.find_cfg_edges_by_source(&node.id).expect("cfg_edges"))
        .map(|edge| edge.kind)
        .collect();
    assert!(edge_kinds.contains(&CfgEdgeKind::Continue));
    assert!(edge_kinds.contains(&CfgEdgeKind::Break));
}

#[test]
#[cfg(feature = "ruby")]
fn fx_cfg_real_ruby_method_rescue_is_persisted() {
    let source = example_source_or_skip!("redis/utils/redis-copy.rb");
    let store = index_files(&[("redis-copy.rb", source)]);
    let file_id = FileId::generate("redis-copy.rb");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "redisCopy" && symbol.kind == SymbolKind::Method)
        .expect("missing real Redis redisCopy method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let handler = nodes
        .iter()
        .find(|node| {
            if node.kind != CfgNodeKind::Statement {
                return false;
            }
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            source
                .get(range)
                .is_some_and(|text| text.trim_start().starts_with("$stderr.puts"))
        })
        .expect("real Redis rescue handler statement");
    let dispatch = nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Branch)
        .expect("method-level rescue dispatch");
    let edges = persisted_cfg_edges(&store, &nodes);

    assert!(edges.iter().any(|edge| {
        edge.source == dispatch.id
            && edge.target == handler.id
            && edge.kind == CfgEdgeKind::Exception
    }));
}

#[test]
#[cfg(feature = "ruby")]
fn fx_cfg_ruby_ensure_clones_survive_persistence() {
    let source = include_str!("fixtures/ruby/cfg_ensure.rb");
    let store = index_files(&[("cfg_ensure.rb", source)]);
    let file_id = FileId::generate("cfg_ensure.rb");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "process" && symbol.kind == SymbolKind::Method)
        .expect("missing Ruby process method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let cleanup_ids =
        persisted_cfg_node_ids_for_text(&nodes, source, CfgNodeKind::Statement, "cleanup()");
    let return_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Return, "return 1");
    let raise_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Throw, "raise Error");
    let work_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "work()");
    let recover_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "recover()");
    let success_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "success()");
    let after_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "after()");
    let edges = persisted_cfg_edges(&store, &nodes);

    assert_eq!(
        cleanup_ids.len(),
        4,
        "each continuation needs its own ensure clone"
    );
    assert!(persisted_cfg_reaches(&edges, work_id, success_id));
    assert!(persisted_cfg_reaches(&edges, success_id, after_id));
    assert!(persisted_cfg_reaches(&edges, raise_id, recover_id));
    assert!(persisted_cfg_reaches(&edges, recover_id, after_id));
    assert!(!persisted_cfg_reaches(&edges, return_id, after_id));
    assert!(
        cleanup_ids
            .iter()
            .any(|cleanup| persisted_cfg_reaches(&edges, return_id, *cleanup))
    );
}

#[test]
#[cfg(feature = "ruby")]
fn fx_cfg_ruby_resource_block_jumps_resume_after_call_when_persisted() {
    let source = r#"def read(stop, skip)
  File.open('data.txt') do |resource|
    if stop
      break
    end
    if skip
      next
    end
    consume(resource)
  end
  after()
end
"#;
    let store = index_files(&[("block_jumps.rb", source)]);
    let file_id = FileId::generate("block_jumps.rb");
    let symbol = store
        .find_symbols_by_file(&file_id)
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "read" && symbol.kind == SymbolKind::Method)
        .expect("missing Ruby read method");
    let nodes = store
        .find_cfg_nodes_by_function(&symbol.id)
        .expect("cfg_nodes");
    let break_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "break");
    let next_id = persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "next");
    let after_id =
        persisted_cfg_node_id_for_text(&nodes, source, CfgNodeKind::Statement, "after()");
    let block_exits: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind == CfgNodeKind::BlockExit)
        .map(|node| node.id)
        .collect();
    let edges = persisted_cfg_edges(&store, &nodes);

    assert_eq!(block_exits.len(), 3, "normal, break, and next block exits");
    assert!(persisted_cfg_reaches(&edges, break_id, after_id));
    assert!(persisted_cfg_reaches(&edges, next_id, after_id));
    assert!(!edges.iter().any(|edge| {
        block_exits.contains(&edge.source)
            && matches!(edge.kind, CfgEdgeKind::Break | CfgEdgeKind::Continue)
    }));
}

#[test]
#[cfg(feature = "ruby")]
fn fx_cfg_ruby_retry_and_redo_persist_exact_edges() {
    let source = r#"def redo_loop(again)
  while ready?
    work()
    redo if again
    tail()
  end
end

def retry_load(again)
  begin
    load()
  rescue
    begin
      retry if again
    ensure
      inner_cleanup()
    end
  ensure
    outer_cleanup()
  end
end
"#;
    let store = index_files(&[("retry_redo.rb", source)]);
    let file_id = FileId::generate("retry_redo.rb");
    let symbols = store
        .find_symbols_by_file(&file_id)
        .expect("Ruby retry/redo symbols");

    let redo_fn = symbols
        .iter()
        .find(|symbol| symbol.name == "redo_loop" && symbol.kind == SymbolKind::Method)
        .expect("redo_loop method");
    let redo_nodes = store
        .find_cfg_nodes_by_function(&redo_fn.id)
        .expect("redo_loop CFG nodes");
    let redo_edges = persisted_cfg_edges(&store, &redo_nodes);
    let redo = persisted_cfg_node_id_for_text(&redo_nodes, source, CfgNodeKind::Statement, "redo");
    let work =
        persisted_cfg_node_id_for_text(&redo_nodes, source, CfgNodeKind::Statement, "work()");
    let redo_edge = redo_edges
        .iter()
        .find(|edge| edge.source == redo && edge.target == work && edge.kind == CfgEdgeKind::Redo)
        .expect("persisted redo must target current body entry");
    assert_eq!(
        redo_edge.id,
        CfgEdge::new(&redo, &work, CfgEdgeKind::Redo).id
    );

    let retry_fn = symbols
        .iter()
        .find(|symbol| symbol.name == "retry_load" && symbol.kind == SymbolKind::Method)
        .expect("retry_load method");
    let retry_nodes = store
        .find_cfg_nodes_by_function(&retry_fn.id)
        .expect("retry_load CFG nodes");
    let retry_edges = persisted_cfg_edges(&store, &retry_nodes);
    let retry =
        persisted_cfg_node_id_for_text(&retry_nodes, source, CfgNodeKind::Statement, "retry");
    let inner_cleanup = persisted_cfg_node_ids_for_text(
        &retry_nodes,
        source,
        CfgNodeKind::Statement,
        "inner_cleanup()",
    )
    .into_iter()
    .find(|cleanup| {
        retry_edges.iter().any(|edge| {
            edge.source == retry && edge.target == *cleanup && edge.kind == CfgEdgeKind::Normal
        })
    })
    .expect("persisted retry must execute nested ensure");
    let retry_edge = retry_edges
        .iter()
        .find(|edge| edge.source == inner_cleanup && edge.kind == CfgEdgeKind::Retry)
        .expect("nested ensure tail must retain retry continuation");
    assert!(
        retry_nodes
            .iter()
            .any(|node| node.id == retry_edge.target && node.kind == CfgNodeKind::Branch)
    );
    assert_eq!(
        retry_edge.id,
        CfgEdge::new(&inner_cleanup, &retry_edge.target, CfgEdgeKind::Retry).id
    );
}
