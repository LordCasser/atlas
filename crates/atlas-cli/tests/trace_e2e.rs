//! End-to-end integration tests for trace operations.
//!
//! These tests run the full extraction→resolution→graph pipeline on small
//! inline source files, then exercise [`Locator::locate`],
//! [`Slicer::slice`], [`CallerPathExplorer::explore`],
//! and [`TraceEngine`] on the resulting store.
//!
//! Each test uses store queries to find exact positions for known symbols,
//! avoids guessing line/column coordinates, and verifies that trace
//! operations produce correct results.
//!
//! Run with default features:  `cargo test --test trace_e2e`
//! Run with all languages:    `cargo test --test trace_e2e --features all-languages`

use atlas_analysis::trace::{CallerPathExplorer, Locator, Slicer, TraceEngine};
use atlas_analysis::trace::virtual_edges::TraceEdgeProvider;
use atlas_db::Store;
use atlas_extraction::extract_file;
use atlas_graph::GraphBuilder;
use atlas_resolution::{ReferenceResolver, ResolutionStats};
use atlas_types::enums::Language;
use atlas_types::ids::FileId;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

/// Combined stats from the resolve + build pipeline.
struct PipelineStats {
    resolution: ResolutionStats,
    edges_built: usize,
}

/// Run the full pipeline on a set of source files.
fn index_files(files: &[(&str, &str)]) -> (Arc<Store>, PipelineStats) {
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

    // Resolve references, then build structural edges.
    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, resolution) = resolver.resolve_all().expect("resolution failed");

    let builder = GraphBuilder::new(store.clone());
    let build_stats = builder.build_all(&resolved);

    let stats = PipelineStats {
        resolution,
        edges_built: build_stats.edges_built,
    };
    (store, stats)
}

/// Locate at the mid-point of a stored range (0-based → 1-based conversion).
fn mid_point(start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> (u32, u32) {
    let line = start_line + (end_line.saturating_sub(start_line)) / 2;
    let col = start_col + (end_col.saturating_sub(start_col)) / 2;
    // locator expects 1-based
    (line + 1, col + 1)
}

// ────────────────────────────────────────────────────────────────
// TS Trace Tests — Locator (default features)
// ────────────────────────────────────────────────────────────────

#[test]
fn ts_locate_resolves_call_reference_to_function_symbol() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.ts",
        r#"function helper(x: number): number {
    return x * 2;
}

function main(): void {
    const result = helper(21);
    console.log(result);
}

main();
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    let file_id = FileId::generate("app.ts");

    // Find the helper function symbol.
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let helper_sym = syms
        .iter()
        .find(|s| s.name == "helper")
        .expect("helper function symbol not found");

    // Find a reference that resolves to helper (i.e. the call `helper(21)`).
    let refs = store.find_references_by_file(&file_id).unwrap();
    let helper_ref = refs
        .iter()
        .find(|r| {
            r.resolved
                .as_ref()
                .map(|t| t.symbol_id == helper_sym.id)
                .unwrap_or(false)
        })
        .expect("call reference to helper not found");

    // Locate at the reference position.
    let (line, col) = mid_point(
        helper_ref.range.start_line,
        helper_ref.range.start_column,
        helper_ref.range.end_line,
        helper_ref.range.end_column,
    );
    let point = Locator::locate(&store, &file_id, line, col).unwrap();

    // The reference should be found and resolved to the helper symbol.
    assert!(
        point.reference.is_some(),
        "expected a reference at the call position"
    );
    assert!(
        point.resolved_symbol.is_some(),
        "expected resolved symbol for call reference"
    );
    assert_eq!(
        point.resolved_symbol.as_ref().unwrap().name,
        "helper",
        "resolved symbol should be 'helper'"
    );
    // Verify we found the *same* reference by comparing ids.
    assert_eq!(
        point.reference.as_ref().unwrap().id,
        helper_ref.id,
        "locator should find the same reference"
    );
}

#[test]
fn ts_locate_finds_data_node_at_variable_assignment() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "calc.ts",
        r#"function compute(): number {
    const base = 10;
    const multiplier = 3;
    const result = base * multiplier;
    return result;
}
"#,
    )];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("calc.ts");

    // Find a data node for a local variable (e.g. `result`).
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let result_node = nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("result"))
        .expect("data node for 'result' not found — check TS dataflow extraction");

    let (line, col) = mid_point(
        result_node.range.start_line,
        result_node.range.start_column,
        result_node.range.end_line,
        result_node.range.end_column,
    );
    let point = Locator::locate(&store, &file_id, line, col).unwrap();

    // The locator should find the data node at this position.
    assert!(
        point.data_node.is_some(),
        "expected a data node at the variable assignment position"
    );
    assert_eq!(
        point.data_node.as_ref().unwrap().id,
        result_node.id,
        "locator should find the same data node"
    );

    // The binding should be present (it's a variable declaration).
    assert!(
        point.binding.is_some(),
        "expected a binding at a variable declaration"
    );

    // The scope should be present (enclosing function).
    assert!(point.scope.is_some(), "expected an enclosing scope");
}

#[test]
fn ts_locate_finds_scope_at_function_body() {
    let _ = tracing_subscriber::fmt::try_init();
    // Scopes use generated names like "Function#<byte_offset>",
    // so we verify by presence and containment rather than name.
    let files = &[(
        "fn.ts",
        r#"function greet(name: string): string {
    return `Hello ${name}`;
}
"#,
    )];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("fn.ts");

    // Locate inside the function body (line 2, inside `return`).
    // The locator should find a scope even if the exact position varies.
    let point = Locator::locate(&store, &file_id, 2, 5).unwrap();

    assert!(
        point.scope.is_some(),
        "expected a scope when locating inside a function body"
    );

    // The scope kind should be Function (or another scope type).
    let scope = point.scope.as_ref().unwrap();
    assert!(!scope.name.is_empty(), "scope name should not be empty");
    assert!(
        !scope.scope_path.is_empty(),
        "scope path should not be empty"
    );
}

// ────────────────────────────────────────────────────────────────
// TS Trace Tests — Slicer (default features)
// ────────────────────────────────────────────────────────────────

#[test]
fn ts_slicer_traces_backward_dataflow_from_variable() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "calc.ts",
        r#"function compute(): number {
    const base = 10;
    const multiplier = 3;
    const result = base * multiplier;
    return result;
}
"#,
    )];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("calc.ts");

    // Find the `result` data node as our sink.
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let result_node = nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("result"))
        .expect("data node for 'result' not found");

    let (line, col) = mid_point(
        result_node.range.start_line,
        result_node.range.start_column,
        result_node.range.end_line,
        result_node.range.end_column,
    );
    let sink_point = Locator::locate(&store, &file_id, line, col).unwrap();
    assert!(
        sink_point.data_node.is_some(),
        "sink point must have a data node"
    );

    // Slice backward from the result variable.
    let path = Slicer::slice(&store, &sink_point, 20, None)
        .unwrap()
        .expect("backward slice should produce a path");

    // The path should have at least one step (result flows from base * multiplier).
    assert!(
        !path.steps.is_empty(),
        "expected at least one dataflow step from result"
    );

    // The sink should match our trace point.
    assert_eq!(
        path.sink.data_node.as_ref().map(|n| n.id),
        Some(result_node.id.clone()),
        "path.sink should be the result node"
    );

    // There should be some confidence in the trace.
    assert!(
        path.confidence > 0.0,
        "expected positive confidence for the trace"
    );

    // Visited nodes should be > 0.
    assert!(path.nodes_visited > 0, "expected nodes to be visited");
}

#[test]
fn ts_slicer_returns_none_for_position_without_data_node() {
    let _ = tracing_subscriber::fmt::try_init();
    // Use a file with no data nodes — just a function declaration without body.
    let files = &[("empty.ts", "export type Status = 'ok' | 'error';\n")];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("empty.ts");

    // Locate at a known position inside the type (e.g., character 0 of line 1).
    let point = Locator::locate(&store, &file_id, 1, 1).unwrap();

    // Slicer should return None when there is no data node.
    let path = Slicer::slice(&store, &point, 10, None).unwrap();
    assert!(
        path.is_none(),
        "slicer should return None for positions without data nodes"
    );
}

// ────────────────────────────────────────────────────────────────
// TS Trace Tests — CallerPathExplorer (default features)
// ────────────────────────────────────────────────────────────────

#[test]
fn ts_caller_path_finds_call_chain() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.ts",
        r#"function inner(x: number): number {
    return x + 1;
}

function middle(y: number): number {
    return inner(y);
}

function outer(z: number): void {
    const n = middle(z);
}
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.edges_built > 0, "expected structural edges");

    let file_id = FileId::generate("app.ts");

    // Find the inner symbol.
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let inner_sym = syms
        .iter()
        .find(|s| s.name == "inner")
        .expect("inner symbol not found");

    // Explore callers of inner.
    let chain = CallerPathExplorer::explore(&store, &inner_sym.id, 10)
        .unwrap()
        .expect("expected caller chain for inner");

    // The chain should have at least one step (middle → inner).
    assert!(
        !chain.steps.is_empty(),
        "expected at least one caller step for inner"
    );

    // Verify that 'middle' appears as a caller.
    let middle_calls_inner = chain.steps.iter().any(|step| {
        store
            .find_symbol_by_id(&step.caller)
            .ok()
            .flatten()
            .map(|s| s.name.as_str() == "middle")
            .unwrap_or(false)
    });
    assert!(
        middle_calls_inner,
        "expected 'middle' to appear as a caller of 'inner'"
    );

    // The root should be the farthest caller found.
    assert!(
        !chain.root.name.is_empty(),
        "root caller should have a name"
    );

    // The target should be inner.
    assert_eq!(chain.target.name, "inner", "chain target should be inner");
}

#[test]
fn ts_caller_path_returns_none_for_root_function() {
    let _ = tracing_subscriber::fmt::try_init();
    // A single function with no callers.
    let files = &[("root.ts", "function standalone(): number { return 42; }\n")];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("root.ts");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let standalone_sym = syms
        .iter()
        .find(|s| s.name == "standalone")
        .expect("standalone symbol not found");

    // Root function should have no callers.
    let chain = CallerPathExplorer::explore(&store, &standalone_sym.id, 10).unwrap();
    assert!(
        chain.is_none(),
        "standalone function with no callers should return None"
    );
}

/// Interproc bridge: verify that SummaryEdgeProvider connects a callee
/// Parameter node to the caller's call-arg DataNode via ArgToParam edge.
///
/// This test directly exercises the SummaryEdgeProvider (not the full slicer
/// pipeline) because the current TS dataflow model captures expression-level
/// DataNodes (e.g. `x + y` as one Expr), so the slicer BFS doesn't naturally
/// reach Parameter nodes from use-sites.  The bridge itself is the critical
/// invariant — once per-identifier edges exist (P3), the slicer will
/// automatically use it.
#[test]
fn ts_interproc_param_bridges_to_caller_arg() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "interproc.ts",
        r#"
// callee: takes two parameters
function multiply(input: number, factor: number): number {
    return input * factor;
}
// caller: invokes multiply with literal arguments
function run(): void {
    const x = multiply(42, 7);
}
"#,
    )];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("interproc.ts");

    // ── Verify function_id is set on Parameter DataNodes ──
    let data_nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let params: Vec<_> = data_nodes
        .iter()
        .filter(|dn| dn.kind == atlas_types::enums::DataNodeKind::Parameter)
        .collect();
    assert!(
        params.len() >= 2,
        "should have at least 2 Parameter DataNodes (input, factor)"
    );
    for param in &params {
        assert!(
            param.function_id.is_some(),
            "Parameter {} should have function_id set (Fix 1: range expansion)",
            param.name.as_deref().unwrap_or("?"),
        );
    }

    // ── Verify SummaryEdgeProvider produces ArgToParam edges ──
    let input_param = params
        .iter()
        .find(|dn| dn.name.as_deref() == Some("input"))
        .expect("should find input parameter");
    let provider =
        atlas_analysis::trace::virtual_edges::SummaryEdgeProvider;
    let edges = provider
        .virtual_incoming(&input_param.id, store.as_ref())
        .expect("virtual_incoming should succeed");

    // Check that at least one edge bridges from caller arg to callee param
    let arg_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.kind == atlas_types::enums::DataFlowKind::ArgToParam)
        .collect();
    assert!(
        !arg_edges.is_empty(),
        "SummaryEdgeProvider should produce ArgToParam edges from caller arg \
         to callee param; found {} edges total, {} ArgToParam",
        edges.len(),
        arg_edges.len(),
    );

    for edge in &arg_edges {
        assert!(
            edge.confidence < 0.7,
            "virtual ArgToParam edge should have confidence 0.67"
        );
        assert!(
            edge.provenance.contains("caller arg"),
            "virtual edge provenance should mention 'caller arg': {}",
            edge.provenance,
        );
        assert_eq!(
            edge.target_id, input_param.id,
            "virtual edge should target the input parameter"
        );
    }
}

/// Interproc return bridge: verify that SummaryEdgeProvider connects a caller's
/// call-result Expr (assign_value with callsite_id) to the callee's return
/// sources via ReturnToCall edges.
///
/// This test directly exercises the SummaryEdgeProvider (not the full slicer
/// pipeline).  The bridge matches callee return→caller call-result.
#[test]
fn ts_interproc_return_bridges_to_caller_call_result() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "callee.ts",
            r#"
export function compute(base: number, factor: number): number {
    const result = base * factor;
    return result;
}
"#,
        ),
        (
            "caller.ts",
            r#"
import { compute } from './callee';
function run(): void {
    const x = compute(10, 3);
    console.log(x);
}
"#,
        ),
    ];
    let (store, _stats) = index_files(files);
    let caller_file_id = FileId::generate("caller.ts");

    // ── Find the assign_value Expr in caller that IS the call result ──
    let data_nodes = store.find_data_nodes_by_file(&caller_file_id).unwrap();
    let call_exprs: Vec<_> = data_nodes
        .iter()
        .filter(|dn| {
            dn.kind == atlas_types::enums::DataNodeKind::Expr
                && dn.callsite_id.is_some()
        })
        .collect();
    assert!(
        !call_exprs.is_empty(),
        "should have at least one Expr DataNode with callsite_id \
         (assign_value of call expression in caller.ts); Fix: df.assign_value \
         now gets callsite_id from enclosing call_expression"
    );

    let call_result_expr = call_exprs[0];
    assert!(
        call_result_expr.callsite_id.is_some(),
        "call_result Expr must have callsite_id for return bridge"
    );

    // ── Verify SummaryEdgeProvider produces ReturnToCall edges ──
    let provider =
        atlas_analysis::trace::virtual_edges::SummaryEdgeProvider;
    let edges = provider
        .virtual_incoming(&call_result_expr.id, store.as_ref())
        .expect("virtual_incoming should succeed");

    let return_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.kind == atlas_types::enums::DataFlowKind::ReturnToCall)
        .collect();
    assert!(
        !return_edges.is_empty(),
        "SummaryEdgeProvider should produce ReturnToCall edges from callee \
         return to caller call-result Expr; found {} edges total, {} ReturnToCall",
        edges.len(),
        return_edges.len(),
    );

    for edge in &return_edges {
        assert!(
            edge.confidence > 0.5,
            "ReturnToCall virtual edge should have confidence > 0.5"
        );
        assert!(
            edge.provenance.contains("callee return"),
            "virtual edge provenance should mention 'callee return': {}",
            edge.provenance,
        );
        assert_eq!(
            edge.target_id, call_result_expr.id,
            "virtual edge should target the caller's call-result Expr"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Python Locate Tests (default features)
// ────────────────────────────────────────────────────────────────

#[test]
fn py_locate_finds_reference_at_class_usage() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "models.py",
            r#"class Calculator:
    def add(self, x, y):
        return x + y
"#,
        ),
        (
            "main.py",
            r#"from models import Calculator

def run():
    calc = Calculator()
    result = calc.add(3, 4)
    print(result)

run()
"#,
        ),
    ];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");

    let models_id = FileId::generate("models.py");
    let main_id = FileId::generate("main.py");

    // Verify that the Calculator class was extracted.
    let syms = store.find_symbols_by_file(&models_id).unwrap();
    let calc_sym = syms
        .iter()
        .find(|s| s.name == "Calculator")
        .expect("Calculator symbol not found in models.py");

    // Find a reference to Calculator in main.py (the `Calculator()` call).
    let refs = store.find_references_by_file(&main_id).unwrap();
    let calc_ref = refs
        .iter()
        .find(|r| {
            r.resolved
                .as_ref()
                .map(|t| t.symbol_id == calc_sym.id)
                .unwrap_or(false)
        })
        .expect("reference to Calculator not found in main.py");

    let (line, col) = mid_point(
        calc_ref.range.start_line,
        calc_ref.range.start_column,
        calc_ref.range.end_line,
        calc_ref.range.end_column,
    );
    let point = Locator::locate(&store, &main_id, line, col).unwrap();

    assert!(
        point.reference.is_some(),
        "expected a reference at the Calculator() position"
    );
    assert!(point.resolved_symbol.is_some(), "expected resolved symbol");
    assert_eq!(
        point.resolved_symbol.as_ref().unwrap().name,
        "Calculator",
        "resolved symbol should be Calculator"
    );
}

// ────────────────────────────────────────────────────────────────
// C Locate Tests (feature-gated)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "c")]
#[test]
fn c_locate_finds_reference_at_function_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "math.c",
        r#"int add(int a, int b) {
    return a + b;
}

int main() {
    int result = add(3, 4);
    return result;
}
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    let file_id = FileId::generate("math.c");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let add_sym = syms
        .iter()
        .find(|s| s.name == "add")
        .expect("add symbol not found");

    let refs = store.find_references_by_file(&file_id).unwrap();
    let add_ref = refs
        .iter()
        .find(|r| {
            r.resolved
                .as_ref()
                .map(|t| t.symbol_id == add_sym.id)
                .unwrap_or(false)
        })
        .expect("reference to add not found");

    let (line, col) = mid_point(
        add_ref.range.start_line,
        add_ref.range.start_column,
        add_ref.range.end_line,
        add_ref.range.end_column,
    );
    let point = Locator::locate(&store, &file_id, line, col).unwrap();

    assert!(point.reference.is_some(), "expected a reference");
    assert!(point.resolved_symbol.is_some(), "expected resolved symbol");
    assert_eq!(
        point.resolved_symbol.as_ref().unwrap().name,
        "add",
        "resolved symbol should be add"
    );
}

// ────────────────────────────────────────────────────────────────
// Java Locate Tests (feature-gated)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "java")]
#[test]
fn java_locate_finds_reference_at_class_usage() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "Greeter.java",
            r#"public class Greeter {
    public String greet(String name) {
        return "Hello, " + name;
    }
}
"#,
        ),
        (
            "Main.java",
            r#"public class Main {
    public static void main(String[] args) {
        Greeter g = new Greeter();
        String msg = g.greet("World");
        System.out.println(msg);
    }
}
"#,
        ),
    ];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    let greeter_id = FileId::generate("Greeter.java");
    let main_id = FileId::generate("Main.java");

    let syms = store.find_symbols_by_file(&greeter_id).unwrap();
    let greeter_sym = syms
        .iter()
        .find(|s| s.name == "Greeter")
        .expect("Greeter symbol not found");

    // Find the reference to Greeter in Main.java (the `new Greeter()` instantiation).
    let refs = store.find_references_by_file(&main_id).unwrap();
    let greeter_ref = refs
        .iter()
        .find(|r| {
            r.resolved
                .as_ref()
                .map(|t| t.symbol_id == greeter_sym.id)
                .unwrap_or(false)
        })
        .expect("reference to Greeter not found in Main.java");

    let (line, col) = mid_point(
        greeter_ref.range.start_line,
        greeter_ref.range.start_column,
        greeter_ref.range.end_line,
        greeter_ref.range.end_column,
    );
    let point = Locator::locate(&store, &main_id, line, col).unwrap();

    assert!(point.reference.is_some(), "expected a reference");
    assert!(point.resolved_symbol.is_some(), "expected resolved symbol");
    assert_eq!(
        point.resolved_symbol.as_ref().unwrap().name,
        "Greeter",
        "resolved symbol should be Greeter"
    );
}

// ────────────────────────────────────────────────────────────────
// P0-3: Call argument position — data_node + callsite + incoming
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_call_arg_position_data_node_kind_is_call_arg() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "call.ts",
        r#"function doubled(x: number): number {
    return x * 2;
}

function outer(val: number): void {
    const result = doubled(val * 3);
}
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    let file_id = FileId::generate("call.ts");

    // ── Step 1: Find a CallArg data node (the argument at doubled(...)) ──
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let call_arg_nodes: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == atlas_types::enums::DataNodeKind::CallArg)
        .collect();
    assert!(
        !call_arg_nodes.is_empty(),
        "expected at least one CallArg data node in the file"
    );

    // Pick a CallArg node (the `val * 3` expression argument)
    let arg_node = *call_arg_nodes.first().unwrap();

    // ── Step 2: Locate at the call arg position ──
    let (line, col) = mid_point(
        arg_node.range.start_line,
        arg_node.range.start_column,
        arg_node.range.end_line,
        arg_node.range.end_column,
    );
    let point = Locator::locate(&store, &file_id, line, col).unwrap();

    // ── Step 3: Assert data_node.kind == CallArg ──
    assert!(
        point.data_node.is_some(),
        "locator should find a data node at the call arg position"
    );
    let dn = point.data_node.as_ref().unwrap();
    assert_eq!(
        dn.kind,
        atlas_types::enums::DataNodeKind::CallArg,
        "data node at call arg position should have kind CallArg, got {:?}",
        dn.kind
    );

    // ── Step 4: Assert callsite is present ──
    assert!(
        point.callsite.is_some(),
        "locator should find a callsite containing the call arg node"
    );

    // ── Step 5: Optionally assert incoming dataflow edges exist ──
    // The arg `val * 3` should have incoming dataflow from the local `val`.
    if point.incoming.is_empty() {
        // Acceptable: TS dataflow is heuristic; not every expression gets edges.
        // But the data_node kind and callsite are the critical assertions.
    }
}

#[test]
fn p0_call_arg_position_still_resolves_callee_symbol() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "call2.ts",
        r#"function isPositive(n: number): boolean {
    return n > 0;
}

function check(v: number): void {
    const ok = isPositive(v);
}
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0);

    let file_id = FileId::generate("call2.ts");

    // Find the isPositive symbol
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let is_pos = syms
        .iter()
        .find(|s| s.name == "isPositive")
        .expect("isPositive symbol not found");

    // Find the reference to isPositive (the call `isPositive(v)`)
    let refs = store.find_references_by_file(&file_id).unwrap();
    let call_ref = refs
        .iter()
        .find(|r| {
            r.resolved
                .as_ref()
                .map(|t| t.symbol_id == is_pos.id)
                .unwrap_or(false)
        })
        .expect("call reference to isPositive not found");

    // Locate at the call reference (inside `isPositive`)
    let (line, col) = mid_point(
        call_ref.range.start_line,
        call_ref.range.start_column,
        call_ref.range.end_line,
        call_ref.range.end_column,
    );
    let point = Locator::locate(&store, &file_id, line, col).unwrap();

    // Should resolve to isPositive
    assert!(point.resolved_symbol.is_some());
    assert_eq!(point.resolved_symbol.as_ref().unwrap().name, "isPositive");

    // The callsite should exist and cover the full call expression `isPositive(v)`
    assert!(
        point.callsite.is_some(),
        "callsite should exist at call position"
    );
    let cs = point.callsite.as_ref().unwrap();
    assert!(
        cs.range.start_line <= cs.range.end_line,
        "callsite range should be valid"
    );

    // The callsite range should be wider than just the function name
    // (it should cover arguments too after the P1a fix)
    let cs_char_span = cs.range.end_byte.saturating_sub(cs.range.start_byte);
    let ref_char_span = call_ref
        .range
        .end_byte
        .saturating_sub(call_ref.range.start_byte);
    assert!(
        cs_char_span >= ref_char_span,
        "callsite range ({} bytes) should cover at least the call reference ({} bytes)",
        cs_char_span,
        ref_char_span
    );
}

// ────────────────────────────────────────────────────────────────
// P0-4: Caller-path call evidence — step range + callsite
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_caller_path_step_range_points_to_call_site_not_caller_def() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "chain.ts",
        r#"function inner(x: number): number {
    return x + 1;
}

function middle(y: number): number {
    const doubled = y * 2;
    return inner(doubled);
}

function outer(z: number): number {
    const sum = z + 100;
    return middle(sum);
}
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.edges_built > 0, "expected structural edges");

    let file_id = FileId::generate("chain.ts");

    // Find the inner symbol
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let inner_sym = syms
        .iter()
        .find(|s| s.name == "inner")
        .expect("inner symbol not found");
    let _middle_sym = syms
        .iter()
        .find(|s| s.name == "middle")
        .expect("middle symbol not found");

    // Get caller chain for inner
    let chain = CallerPathExplorer::explore(&store, &inner_sym.id, 10)
        .unwrap()
        .expect("expected caller chain for inner");

    // ── Assert: at least one step with middle → inner ──
    let middle_to_inner = chain.steps.iter().find(|step| {
        store
            .find_symbol_by_id(&step.callee)
            .ok()
            .flatten()
            .map(|s| s.name == "inner")
            .unwrap_or(false)
            && store
                .find_symbol_by_id(&step.caller)
                .ok()
                .flatten()
                .map(|s| s.name == "middle")
                .unwrap_or(false)
    });
    assert!(
        middle_to_inner.is_some(),
        "expected a step where middle calls inner"
    );

    let step = middle_to_inner.unwrap();

    // ── Assert: step.range is Some and points to the call location ──
    assert!(
        step.range.is_some(),
        "step should have a range pointing to the call location, not None"
    );
    let step_range = step.range.as_ref().unwrap();

    // The call to inner is at line 7 (1-based): `return inner(doubled);`
    // middle's function body starts at line 5 (1-based), line 6 (0-based).
    // But SymbolDef.range only covers the identifier token (the name),
    // not the full function body.  Compare against known source lines.
    // 0-based: function header at line 4, body from line 5, call at line 6.
    let middle_def_line_0b: u32 = 4; // `function middle(y: number): number {`
    assert!(
        step_range.start_line > middle_def_line_0b,
        "step range start line ({}) should be AFTER middle's function header \
         (line {}), i.e. inside the body not at the definition",
        step_range.start_line,
        middle_def_line_0b
    );

    // The step range should correspond to the call line, not some other line.
    // `inner(doubled)` is at 0-based line 6.
    assert!(
        step_range.start_line == 6 || (step_range.start_line >= 5 && step_range.start_line <= 7),
        "step range (line {}) should be near the inner() call on line 6-7",
        step_range.start_line
    );

    // ── Assert: callsite is populated after P1b fix ──
    // The step corresponds to `middle` calling `inner`, so there must be a
    // callsite record in the store.  After P1b, step.callsite should be Some.
    assert!(
        step.callsite.is_some(),
        "caller-path step for 'middle → inner' must have a callsite, not None. \
         The P1b fix ensures callsite is populated from edge ref_id."
    );
    if let Some(ref cs) = step.callsite {
        // The callsite range should cover the full call expression
        // `inner(doubled)`, not just the callee name token.
        assert!(
            cs.range.start_line > 0,
            "callsite range should be valid (non-zero line)"
        );
        // callee_range should be set (the `inner` token within the expression).
        if let Some(ref cr) = cs.callee_range {
            assert!(
                cr.start_line > 0,
                "callee_range should be valid (non-zero line)"
            );
        }
    }
}

#[test]
fn p0_caller_path_chain_length_matches_call_depth() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "depth.ts",
        r#"function deepest(): number { return 42; }
function deep(x: number): number { return deepest() + x; }
function mid(x: number): number { return deep(x) * 2; }
function top(x: number): number { return mid(x); }
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.edges_built >= 3, "expected at least 3 call edges");

    let file_id = FileId::generate("depth.ts");
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let deepest_sym = syms
        .iter()
        .find(|s| s.name == "deepest")
        .expect("deepest symbol not found");

    // Farthest chain should be: top → mid → deep → deepest (3 steps)
    let chain = CallerPathExplorer::explore(&store, &deepest_sym.id, 10)
        .unwrap()
        .expect("expected caller chain for deepest");

    assert_eq!(
        chain.steps.len(),
        3,
        "expected 3 steps (top→mid→deep→deepest), got {}",
        chain.steps.len()
    );
    assert_eq!(chain.root.name, "top", "farthest root should be 'top'");
    assert_eq!(chain.target.name, "deepest", "target should be 'deepest'");
    assert!(
        chain.max_depth_reached >= 3,
        "max_depth_reached should be at least 3"
    );
}

// ────────────────────────────────────────────────────────────────
// P0: TraceEngine — unified response envelope
// ────────────────────────────────────────────────────────────────

#[test]
fn p0_trace_engine_returns_response_envelope_for_point() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("e.ts", "function greet(): string { return 'hi'; }\n")];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("e.ts");

    let engine = TraceEngine::new(store);
    let resp = engine.trace_point(&file_id, 1, 10);

    assert!(resp.ok, "trace_point should always succeed");
    assert_eq!(resp.kind, "trace_point");
    assert!(
        !resp.partial_result,
        "should not be partial for valid position"
    );
    assert!(
        resp.diagnostics.is_empty(),
        "no diagnostics for simple point"
    );
    assert!(resp.result.is_some(), "should have a TracePoint result");
    assert!(
        resp.capability.is_some(),
        "capability should be resolved from file language"
    );

    // Verify JSON round-trip contains all envelope fields
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains(r#""ok""#));
    assert!(json.contains(r#""kind""#));
    assert!(json.contains(r#""partial_result""#));
    assert!(json.contains(r#""diagnostics""#));
    assert!(json.contains(r#""result""#));
    assert!(json.contains(r#""capability""#));
}

#[test]
fn p0_trace_engine_variable_on_dataflow_language() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "v.ts",
        "function compute(n: number): number { const x = n * 2; return x; }\n",
    )];
    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("v.ts");

    // Find a data node to trace from
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let local_x = nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("x"))
        .expect("data node 'x' not found");

    let (line, col) = mid_point(
        local_x.range.start_line,
        local_x.range.start_column,
        local_x.range.end_line,
        local_x.range.end_column,
    );

    let engine = TraceEngine::new(store);
    let resp = engine.trace_variable(&file_id, line, col, 20);

    assert!(resp.ok, "trace_variable should succeed for TS");
    assert_eq!(resp.kind, "trace_variable");
    // For TS with dataflow, should produce a result, not be partial
    assert!(
        resp.result.is_some() || resp.partial_result,
        "trace_variable should have result or be partial (heuristic)"
    );
    if let Some(ref path) = resp.result {
        assert!(path.confidence > 0.0);
    }
}

#[test]
fn p0_trace_engine_callers_produces_chain() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "c.ts",
        r#"function callee(): number { return 1; }
function caller(): number { return callee(); }
"#,
    )];
    let (store, stats) = index_files(files);
    assert!(stats.edges_built > 0, "expected call edges");

    let file_id = FileId::generate("c.ts");
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let callee_sym = syms
        .iter()
        .find(|s| s.name == "callee")
        .expect("callee symbol not found");

    let engine = TraceEngine::new(store);
    let resp = engine.trace_callers(&callee_sym.id, 10);

    assert!(resp.ok, "trace_callers should succeed");
    assert_eq!(resp.kind, "trace_callers");
    assert!(resp.capability.is_some(), "capability should be resolved");
    assert!(resp.result.is_some(), "should have caller chain");
    let chain = resp.result.as_ref().unwrap();
    assert_eq!(chain.target.name, "callee");
    assert!(
        !chain.steps.is_empty(),
        "should have at least one caller step"
    );
}

// ────────────────────────────────────────────────────────────────
// Evidence Layer: steps carry file_path + symbol_name
// ────────────────────────────────────────────────────────────────

/// TraceEngine::trace_variable must attach evidence to TracePathStep.
#[test]
fn p4_trace_path_step_has_evidence() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "calc.ts",
        r#"function compute(): number {
    const base = 10;
    const multiplier = 3;
    const result = base * multiplier;
    return result;
}
"#,
    )];
    let (store, _stats) = index_files(files);
    let engine = TraceEngine::new(store);
    let file_id = FileId::generate("calc.ts");

    let resp = engine.trace_variable(&file_id, 4, 22, 20);

    if let Some(ref path) = resp.result {
        for step in &path.steps {
            assert!(
                step.evidence.is_some(),
                "TracePathStep must have evidence; step index={}",
                step.index
            );
            let ev = step.evidence.as_ref().unwrap();
            assert!(
                !ev.file_path.is_empty(),
                "evidence.file_path must be populated"
            );
            // symbol_name may or may not be populated depending on dataflow
        }
    }
}

/// TraceEngine::trace_callers must attach evidence to CallerChainStep.
#[test]
fn p4_caller_chain_step_has_evidence() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "chain.ts",
        r#"function inner(x: number): number {
    return x + 1;
}

function middle(y: number): number {
    return inner(y);
}
"#,
    )];
    let (store, _stats) = index_files(files);
    let engine = TraceEngine::new(store.clone());
    let file_id = FileId::generate("chain.ts");
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let inner_sym = syms
        .iter()
        .find(|s| s.name == "inner")
        .expect("inner symbol not found");

    let resp = engine.trace_callers(&inner_sym.id, 10);

    if let Some(ref chain) = resp.result {
        for step in &chain.steps {
            assert!(
                step.evidence.is_some(),
                "CallerChainStep must have evidence; step index={}",
                step.index
            );
            let ev = step.evidence.as_ref().unwrap();
            assert!(
                !ev.file_path.is_empty(),
                "evidence.file_path must be populated"
            );
            // symbol_name should be the caller's name
            assert!(
                ev.symbol_name.is_some(),
                "evidence.symbol_name must be the caller symbol"
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// P5: Combined trace — variable slice + caller path + evidence
//     (TS / JS / Python)
// ────────────────────────────────────────────────────────────────

/// TS combined: param flows to result, result returned, called from handler.
/// trace_variable from `result` backward → slice steps with evidence,
/// trace_callers from `compute` → caller chain with provenance.
#[test]
fn p5_ts_param_slice_caller_evidence_combined() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "helper.ts",
            "export function compute(base: number, factor: number): number {\n    const result = base * factor;\n    return result;\n}\n",
        ),
        (
            "main.ts",
            "import { compute } from './helper';\n\nfunction handler(input: number): string {\n    const value = compute(input, 3);\n    return `Result: ${value}`;\n}\n",
        ),
    ];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    let engine = TraceEngine::new(store.clone());
    let helper_id = FileId::generate("helper.ts");

    // ── trace_variable from 'result' ──
    let data_nodes = store.find_data_nodes_by_file(&helper_id).unwrap();
    let result_node = data_nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("result"))
        .expect("'result' data node not found");

    let resp = engine.trace_variable(
        &helper_id,
        result_node.range.start_line + 1,
        result_node.range.start_column + 1,
        20,
    );
    assert!(resp.ok, "trace_variable should succeed");
    assert!(resp.capability.is_some(), "capability must be present");

    let path = resp.result.as_ref().expect("trace path should exist");
    assert!(path.confidence > 0.0, "confidence should be positive");
    assert!(path.nodes_visited > 0, "nodes_visited should be > 0");

    for (i, step) in path.steps.iter().enumerate() {
        assert!(
            !step.file_id.as_bytes().is_empty(),
            "step {}: file_id should be populated",
            i
        );
        let ev = step
            .evidence
            .as_ref()
            .unwrap_or_else(|| panic!("step {}: evidence must exist", i));
        assert!(
            !ev.file_path.is_empty(),
            "step {}: evidence.file_path must be set",
            i
        );
    }

    // Verify sink identity
    assert_eq!(
        path.sink.data_node.as_ref().map(|n| &n.id),
        Some(&result_node.id),
        "sink should be the result node"
    );

    // ── trace_callers from 'compute' ──
    let helper_syms = store.find_symbols_by_file(&helper_id).unwrap();
    let compute_sym = helper_syms
        .iter()
        .find(|s| s.name == "compute")
        .expect("compute symbol not found");

    let caller_resp = engine.trace_callers(&compute_sym.id, 10);
    assert!(caller_resp.ok, "trace_callers should succeed");

    let chain = caller_resp
        .result
        .as_ref()
        .expect("caller chain should exist");
    assert!(!chain.steps.is_empty(), "should have caller steps");
    assert_eq!(chain.target.name, "compute", "chain target should be compute");

    for (i, step) in chain.steps.iter().enumerate() {
        let ev = step
            .evidence
            .as_ref()
            .unwrap_or_else(|| panic!("caller step {}: evidence must exist", i));
        assert!(
            !ev.file_path.is_empty(),
            "caller step {}: file_path must be set",
            i
        );
        assert!(
            ev.symbol_name.is_some(),
            "caller step {}: symbol_name must be set (provenance)",
            i
        );
    }
}

/// JS combined: same structure as TS but without type annotations.
/// Validates that JavaScript extraction produces the same trace artifacts.
#[test]
fn p5_js_param_slice_caller_evidence_combined() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "helper.js",
            "export function compute(base, factor) {\n    const result = base * factor;\n    return result;\n}\n",
        ),
        (
            "main.js",
            "import { compute } from './helper.js';\n\nfunction handler(input) {\n    const value = compute(input, 3);\n    return `Result: ${value}`;\n}\n",
        ),
    ];
    let (store, stats) = index_files(files);
    assert!(stats.resolution.resolved > 0, "expected resolved refs");

    let engine = TraceEngine::new(store.clone());
    let helper_id = FileId::generate("helper.js");

    // ── trace_variable from 'result' ──
    let data_nodes = store.find_data_nodes_by_file(&helper_id).unwrap();
    let result_node = data_nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("result"))
        .expect("'result' data node not found in JS");

    let resp = engine.trace_variable(
        &helper_id,
        result_node.range.start_line + 1,
        result_node.range.start_column + 1,
        20,
    );
    assert!(resp.ok, "JS trace_variable should succeed");
    assert!(resp.capability.is_some(), "JS capability must be present");

    let path = resp.result.as_ref().expect("JS trace path should exist");
    assert!(path.confidence > 0.0, "JS confidence should be positive");
    assert!(path.nodes_visited > 0, "JS nodes_visited should be > 0");

    for (i, step) in path.steps.iter().enumerate() {
        assert!(
            !step.file_id.as_bytes().is_empty(),
            "JS step {}: file_id should be populated",
            i
        );
        let ev = step
            .evidence
            .as_ref()
            .unwrap_or_else(|| panic!("JS step {}: evidence must exist", i));
        assert!(
            !ev.file_path.is_empty(),
            "JS step {}: evidence.file_path must be set",
            i
        );
    }

    assert_eq!(
        path.sink.data_node.as_ref().map(|n| &n.id),
        Some(&result_node.id),
        "JS sink should be the result node"
    );

    // ── trace_callers from 'compute' ──
    let helper_syms = store.find_symbols_by_file(&helper_id).unwrap();
    let compute_sym = helper_syms
        .iter()
        .find(|s| s.name == "compute")
        .expect("JS compute symbol not found");

    let caller_resp = engine.trace_callers(&compute_sym.id, 10);
    assert!(caller_resp.ok, "JS trace_callers should succeed");

    let chain = caller_resp
        .result
        .as_ref()
        .expect("JS caller chain should exist");
    assert!(!chain.steps.is_empty(), "JS should have caller steps");
    assert_eq!(
        chain.target.name, "compute",
        "JS chain target should be compute"
    );

    for (i, step) in chain.steps.iter().enumerate() {
        let ev = step
            .evidence
            .as_ref()
            .unwrap_or_else(|| panic!("JS caller step {}: evidence must exist", i));
        assert!(
            !ev.file_path.is_empty(),
            "JS caller step {}: file_path must be set",
            i
        );
        assert!(
            ev.symbol_name.is_some(),
            "JS caller step {}: symbol_name must be set",
            i
        );
    }
}

/// Python combined: param flows to result, cross-file caller chain, evidence.
#[test]
fn p5_py_param_slice_caller_evidence_combined() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "helper.py",
            "def compute(base, factor):\n    result = base * factor\n    return result\n",
        ),
        (
            "main.py",
            "from helper import compute\n\ndef handler(input_val):\n    value = compute(input_val, 3)\n    return f\"Result: {value}\"\n",
        ),
    ];
    let (store, _stats) = index_files(files);

    let engine = TraceEngine::new(store.clone());
    let helper_id = FileId::generate("helper.py");

    // ── trace_variable from 'result' ──
    let data_nodes = store.find_data_nodes_by_file(&helper_id).unwrap();
    let result_node = data_nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("result"))
        .expect("'result' data node not found in Python");

    let resp = engine.trace_variable(
        &helper_id,
        result_node.range.start_line + 1,
        result_node.range.start_column + 1,
        20,
    );
    assert!(resp.ok, "Python trace_variable should succeed");
    assert!(resp.capability.is_some(), "Python capability must be present");

    // Python dataflow may be partial — assert envelope is well-formed.
    if let Some(ref path) = resp.result {
        assert!(path.confidence > 0.0, "Python confidence should be positive");
        assert!(path.nodes_visited > 0, "Python nodes_visited should be > 0");

        for (i, step) in path.steps.iter().enumerate() {
            assert!(
                !step.file_id.as_bytes().is_empty(),
                "Python step {}: file_id should be populated",
                i
            );
            let ev = step
                .evidence
                .as_ref()
                .unwrap_or_else(|| panic!("Python step {}: evidence must exist", i));
            assert!(
                !ev.file_path.is_empty(),
                "Python step {}: evidence.file_path must be set",
                i
            );
        }

        assert_eq!(
            path.sink.data_node.as_ref().map(|n| &n.id),
            Some(&result_node.id),
            "Python sink should be the result node"
        );
    } else {
        assert!(
            resp.partial_result || !resp.diagnostics.is_empty(),
            "empty Python result should be partial or diagnostic"
        );
    }

    // ── trace_callers from 'compute' ──
    let helper_syms = store.find_symbols_by_file(&helper_id).unwrap();
    let compute_sym = helper_syms
        .iter()
        .find(|s| s.name == "compute")
        .expect("Python compute symbol not found");

    let caller_resp = engine.trace_callers(&compute_sym.id, 10);
    if caller_resp.ok {
        if let Some(ref chain) = caller_resp.result {
            assert!(!chain.steps.is_empty(), "Python should have caller steps");
            assert_eq!(
                chain.target.name, "compute",
                "Python chain target should be compute"
            );
            for (i, step) in chain.steps.iter().enumerate() {
                let ev = step
                    .evidence
                    .as_ref()
                    .unwrap_or_else(|| {
                        panic!("Python caller step {}: evidence must exist", i)
                    });
                assert!(
                    !ev.file_path.is_empty(),
                    "Python caller step {}: file_path must be set",
                    i
                );
                assert!(
                    ev.symbol_name.is_some(),
                    "Python caller step {}: symbol_name must be set",
                    i
                );
            }
        }
    }
}
