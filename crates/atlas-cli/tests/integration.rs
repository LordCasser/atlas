//! Integration tests for Atlas — end-to-end multi-file pipelines.
//!
//! These tests create temporary directories, write source files, run the
//! full extraction→storage→resolution→graph pipeline, and verify results.
//!
//! Run with default features:  `cargo test --test integration`
//! Run with all languages:    `cargo test --test integration --features all-languages,mcp,sync`

use atlas_engine::Store;
use atlas_engine::extract_file;
use atlas_engine::GraphBuilder;
use atlas_engine::{ReferenceResolver, ResolutionStats};
use atlas_engine::enums::{EdgeKind, Language};
use atlas_engine::ids::FileId;
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

/// Run the full pipeline on a set of source files and return the store + stats.
fn index_files(files: &[(&str, &str)]) -> (Arc<Store>, PipelineStats) {
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

    // P2: two-step pipeline — resolve then build edges
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

// ────────────────────────────────────────────────────────────────
// TS/JS Cross-File Integration Tests (default features)
// ────────────────────────────────────────────────────────────────

#[test]
fn ts_cross_file_import_call_resolves_and_creates_edges() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "lib.ts",
            r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}

export class Formatter {
    format(text: string): string {
        return `[${text}]`;
    }
}
"#,
        ),
        (
            "main.ts",
            r#"import { greet, Formatter } from './lib';

function main() {
    const msg = greet("World");
    const fmt = new Formatter();
    const out = fmt.format(msg);
    console.log(out);
}
main();
"#,
        ),
    ];

    let (store, stats) = index_files(files);

    // Basic resolution stats
    assert!(
        stats.resolution.resolved > 0,
        "expected some resolved refs, got {}",
        stats.resolution.resolved
    );
    assert!(
        stats.edges_built > 0,
        "expected structural edges, got {}",
        stats.edges_built
    );

    // Find the greet symbol from lib.ts
    let lib_id = FileId::generate("lib.ts");
    let lib_syms = store.find_symbols_by_file(&lib_id).unwrap();
    assert!(
        lib_syms.len() >= 2,
        "lib.ts should have at least 2 symbols: greet, Formatter"
    );

    let greet_sym = lib_syms
        .iter()
        .find(|s| s.name == "greet")
        .expect("greet symbol not found");

    // Verify a Calls edge exists pointing to greet
    let all_edges = store.get_all_edges().unwrap();
    let call_edges: Vec<_> = all_edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.target == greet_sym.id)
        .collect();
    assert!(
        !call_edges.is_empty(),
        "expected at least one Calls edge to greet, found 0 among {} total edges",
        all_edges.len()
    );
}

#[test]
fn ts_cross_file_graph_callers_callees() {
    use atlas_engine::GraphEngine;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "utils.ts",
            r#"export function helper(x: number): number {
    return x * 2;
}
"#,
        ),
        (
            "app.ts",
            r#"import { helper } from './utils';

export function process(n: number): number {
    return helper(n) + 1;
}
"#,
        ),
    ];

    let (store, _stats) = index_files(files);

    // Build graph and verify call relationships
    let engine = GraphEngine::from_store(&store, 0.0).unwrap();
    let _snapshot = engine.snapshot();

    let utils_id = FileId::generate("utils.ts");
    let utils_syms = store.find_symbols_by_file(&utils_id).unwrap();
    let helper_sym = utils_syms
        .iter()
        .find(|s| s.name == "helper")
        .expect("helper not found");

    let callers = engine.callers(&helper_sym.id);
    let caller_node_ids = engine.resolve_node_ids(&callers.callers);
    let caller_syms: Vec<_> = caller_node_ids
        .iter()
        .filter_map(|id| store.find_symbol_by_id(id).ok())
        .flatten()
        .collect();
    let caller_names: Vec<_> = caller_syms.iter().map(|s| s.name.as_str()).collect();
    assert!(
        caller_names.contains(&"process"),
        "expected 'process' in callers of 'helper', got {:?}",
        caller_names
    );
}

#[test]
fn ts_scope_tree_assigns_parent_and_container() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "app.ts",
        r#"export class Calculator {
    private base: number = 0;

    add(value: number): number {
        return this.base + value;
    }

    sub(value: number): number {
        return this.base - value;
    }
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("app.ts");

    // Find the Calculator class
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let calc = syms
        .iter()
        .find(|s| s.name == "Calculator")
        .expect("Calculator class not found");
    let add = syms
        .iter()
        .find(|s| s.name == "add")
        .expect("add method not found");
    let sub = syms
        .iter()
        .find(|s| s.name == "sub")
        .expect("sub method not found");

    // Verify container relationships
    assert_eq!(
        add.container,
        Some(calc.id),
        "add.container should be Calculator"
    );
    assert_eq!(
        sub.container,
        Some(calc.id),
        "sub.container should be Calculator"
    );
}

// ────────────────────────────────────────────────────────────────
// Python Integration Tests (default feature)
// ────────────────────────────────────────────────────────────────

#[test]
fn py_cross_file_import_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "lib.py",
            r#"class Calculator:
    def __init__(self, initial=0):
        self.value = initial

    def add(self, x):
        self.value += x
        return self.value

def create_calculator(initial=0):
    return Calculator(initial)
"#,
        ),
        (
            "main.py",
            r#"from lib import Calculator, create_calculator

def main():
    calc = create_calculator()
    result = calc.add(5)
    print(result)

if __name__ == '__main__':
    main()
"#,
        ),
    ];

    let (store, stats) = index_files(files);

    assert!(
        stats.resolution.resolved > 0,
        "expected some resolved refs, got {}",
        stats.resolution.resolved
    );
    assert!(
        stats.edges_built > 0,
        "expected structural edges, got {}",
        stats.edges_built
    );

    // Verify lib.py symbols
    let lib_id = FileId::generate("lib.py");
    let lib_syms = store.find_symbols_by_file(&lib_id).unwrap();
    let names: Vec<_> = lib_syms.iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains(&"Calculator".to_string()),
        "Calculator not found in lib symbols: {:?}",
        names
    );
    assert!(
        names.contains(&"create_calculator".to_string()),
        "create_calculator not found in lib symbols: {:?}",
        names
    );
}

#[test]
fn py_cross_file_class_method_resolution() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "models.py",
            r#"class User:
    def __init__(self, name, email):
        self.name = name
        self.email = email

    def greet(self):
        return f"Hello, {self.name}!"
"#,
        ),
        (
            "app.py",
            r#"from models import User

def run():
    user = User("Alice", "a@b.com")
    msg = user.greet()
    print(msg)

if __name__ == '__main__':
    run()
"#,
        ),
    ];

    let (store, stats) = index_files(files);

    assert!(stats.resolution.resolved > 0, "expected some resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    // Verify User class exists
    let models_id = FileId::generate("models.py");
    let syms = store.find_symbols_by_file(&models_id).unwrap();
    let _user_sym = syms
        .iter()
        .find(|s| s.name == "User")
        .expect("User not found");
    assert!(
        syms.iter().any(|s| s.name == "__init__"),
        "__init__ should be in symbol list"
    );
}

// ────────────────────────────────────────────────────────────────
// Language Detection Tests
// ────────────────────────────────────────────────────────────────

#[test]
fn language_detection_from_path() {
    assert_eq!(
        Language::from_path(Path::new("src/index.ts")),
        Some(Language::TypeScript)
    );
    assert_eq!(
        Language::from_path(Path::new("src/util.js")),
        Some(Language::JavaScript)
    );
    assert_eq!(
        Language::from_path(Path::new("src/app.py")),
        Some(Language::Python)
    );

    #[cfg(feature = "java")]
    assert_eq!(
        Language::from_path(Path::new("src/Main.java")),
        Some(Language::Java)
    );

    #[cfg(feature = "c")]
    assert_eq!(
        Language::from_path(Path::new("src/main.c")),
        Some(Language::C)
    );

    #[cfg(feature = "cpp")]
    assert_eq!(
        Language::from_path(Path::new("src/main.cpp")),
        Some(Language::Cpp)
    );

    #[cfg(feature = "arkts")]
    assert_eq!(
        Language::from_path(Path::new("src/Index.ets")),
        Some(Language::ArkTS)
    );
}

#[test]
fn mixed_language_project_indexes_all_files() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "utils.ts",
            r#"export function ts_util(): string { return "ts"; }
"#,
        ),
        (
            "helper.py",
            r#"def py_util():
    return "py"
"#,
        ),
        (
            "app.ts",
            r#"import { ts_util } from './utils';

function main(): void {
    console.log(ts_util());
}
main();
"#,
        ),
    ];

    let (store, stats) = index_files(files);

    // Verify both TS and Python files were indexed
    let ts_file_id = FileId::generate("utils.ts");
    let py_file_id = FileId::generate("helper.py");

    let ts_syms = store.find_symbols_by_file(&ts_file_id).unwrap();
    let py_syms = store.find_symbols_by_file(&py_file_id).unwrap();

    assert!(!ts_syms.is_empty(), "TS file should have symbols");
    assert!(!py_syms.is_empty(), "Python file should have symbols");

    // Verify resolution happened across files
    assert!(
        stats.resolution.resolved > 0,
        "expected cross-file TS resolution, got {} resolved",
        stats.resolution.resolved
    );
}

// ────────────────────────────────────────────────────────────────
// Edge Verification Tests
// ────────────────────────────────────────────────────────────────

#[test]
fn edge_kinds_are_created_correctly() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "lib.ts",
            r#"export function greet(): string { return "hi"; }

export class Formatter {
    format(): string { return "ok"; }
}
"#,
        ),
        (
            "main.ts",
            r#"import { greet, Formatter } from './lib';

function main(): void {
    const msg = greet();
    const f = new Formatter();
    f.format();
}
main();
"#,
        ),
    ];

    let (store, _stats) = index_files(files);
    let all_edges = store.get_all_edges().unwrap();

    // Should have Calls edges (at least one from main → greet)
    let calls_edges: Vec<_> = all_edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .collect();
    assert!(
        !calls_edges.is_empty(),
        "expected at least 1 Calls edge, got 0"
    );

    // Should have structural edges beyond just raw dataflow edges
    let all_edge_kinds: std::collections::HashSet<&str> =
        all_edges.iter().map(|e| e.kind.as_str()).collect();
    assert!(
        all_edge_kinds.len() >= 2,
        "expected at least 2 different edge kinds, got {:?}",
        all_edge_kinds
    );

    // Should have References edges
    let ref_edges: Vec<_> = all_edges
        .iter()
        .filter(|e| e.kind == EdgeKind::References)
        .collect();
    assert!(
        !ref_edges.is_empty(),
        "expected at least one References edge"
    );
}

// ────────────────────────────────────────────────────────────────
// Search Tests
// ────────────────────────────────────────────────────────────────

#[test]
fn search_finds_symbols_across_files() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "a.ts",
            r#"export function calculateTotal(): number { return 0; }
"#,
        ),
        (
            "b.ts",
            r#"export function calculateAverage(): number { return 0; }
"#,
        ),
        (
            "c.ts",
            r#"export class Calculator {}
"#,
        ),
    ];

    let (store, _stats) = index_files(files);

    // Search for "calculate" should find both functions and the class
    let results = store.search_symbols("calculate").unwrap();
    assert!(
        results.len() >= 2,
        "expected >=2 results for 'calculate', got {}",
        results.len()
    );
}

// ────────────────────────────────────────────────────────────────
// Java/C/C++ Integration Tests (feature-gated)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "java")]
#[test]
fn java_cross_file_import_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "Greeter.java",
            r#"public class Greeter {
    public String greet(String name) {
        return "Hello, " + name + "!";
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

    assert!(stats.resolution.resolved > 0, "expected some resolved refs");
    assert!(stats.edges_built > 0, "expected structural edges");

    let greeter_id = FileId::generate("Greeter.java");
    let syms = store.find_symbols_by_file(&greeter_id).unwrap();
    let names: Vec<_> = syms.iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains(&"Greeter".to_string()),
        "Greeter class not found: {:?}",
        names
    );
}

// ────────────────────────────────────────────────────────────────
// Dataflow TextRange Persistence Tests
// ────────────────────────────────────────────────────────────────

/// Verify that dataflow_edges store complete 6-field TextRange (Task 2).
/// Bug was that only 3 of 6 location fields (byte offsets + start_line)
/// were persisted; start_column, end_line, end_column were always 0.
#[test]
fn ts_dataflow_textrange_complete_roundtrip() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "dataflow.ts",
        r#"function compute(a: number, b: number): number {
    const sum = a + b;
    return sum * 2;
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("dataflow.ts");

    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    assert!(!nodes.is_empty(), "expected data flow nodes");

    // Verify at least one node has complete TextRange (all 6 fields populated)
    let with_full_range: Vec<_> = nodes
        .iter()
        .filter(|n| {
            let r = n.range;
            r.start_byte > 0 || r.end_byte > 0 // at least non-empty
        })
        .collect();
    assert!(
        !with_full_range.is_empty(),
        "expected at least one node with a real byte range"
    );

    // Key assertion: for nodes with real byte ranges, column fields should not
    // all be zero (the original bug stored only 3 of 6 TextRange fields).
    let nodes_with_columns: Vec<_> = with_full_range
        .iter()
        .filter(|n| n.range.start_column > 0 || n.range.end_column > 0)
        .collect();
    assert!(
        !nodes_with_columns.is_empty(),
        "expected at least one data node with non-zero column info. \
         Got {} nodes with byte ranges, but all have start_column=0 end_column=0. \
         This indicates the TextRange column fields (location_3/5) are not being persisted.",
        with_full_range.len()
    );
}

/// Verify that dataflow_edges have complete TextRange after round-trip (Task 2).
#[test]
fn ts_dataflow_edges_complete_textrange() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "compute.ts",
        r#"function triple(x: number): number {
    const y = x * 2;
    return y + x;
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("compute.ts");

    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    // Find edges by source nodes that have a real range
    let mut edge_count = 0;
    for node in &nodes {
        if let Ok(edges) = store.find_dataflow_edges_by_source(&node.id) {
            for edge in &edges {
                edge_count += 1;
                if edge.location.start_byte > 0 || edge.location.end_byte > 0 {
                    // edge has location data
                }
            }
        }
    }

    // We should have at least some dataflow edges in this compute function
    assert!(edge_count > 0, "expected dataflow edges in compute.ts");
    // At minimum, verify edges have non-zero column fields when location exists.
    // (The original bug stored only 3 of 6 TextRange fields for dataflow_edges.)
    let mut edges_with_column = false;
    for node in &nodes {
        if let Ok(edges) = store.find_dataflow_edges_by_source(&node.id) {
            for edge in &edges {
                if edge.location.start_byte > 0 && edge.location.end_byte > 0 {
                    if edge.location.start_column > 0 || edge.location.end_column > 0 {
                        edges_with_column = true;
                    }
                }
            }
        }
    }
    if edge_count > 0 {
        assert!(
            edges_with_column,
            "expected at least one dataflow edge with non-zero column info (start_column/end_column). \
             This indicates the dataflow_edges TextRange column fields (location_3/5) are not being persisted."
        );
    }
}

// ────────────────────────────────────────────────────────────────
// BindingUse Identifier Scanning Tests (Task 11)
// ────────────────────────────────────────────────────────────────

/// Verify that identifier-use scanning produces BindingUse records for
/// variable references (not just declaration sites).
#[test]
fn ts_binding_use_captures_identifier_references() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "uses.ts",
        r#"function process(items: number[]): number {
    let total = 0;
    for (const item of items) {
        total += item;
    }
    return total;
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("uses.ts");

    let uses = store.find_binding_uses_by_file(&file_id).unwrap();
    assert!(
        !uses.is_empty(),
        "expected binding uses in process function"
    );

    // The function has multiple identifiers: total (declared once, used twice),
    // item (declared once, used once), items (parameter), process (function name)
    // We expect more than 1 binding use (at minimum declarations exist)
    let resolved_uses: Vec<_> = uses.iter().filter(|u| u.binding_id.is_some()).collect();
    assert!(
        !resolved_uses.is_empty(),
        "expected some binding uses with resolved binding_id"
    );

    // Verify that at least one binding use references a known binding
    let bindings = store.find_bindings_by_file(&file_id).unwrap();
    assert!(
        !bindings.is_empty(),
        "expected lexical bindings in process function"
    );

    let binding_names: Vec<_> = bindings.iter().map(|b| b.name.clone()).collect();
    let use_names: Vec<_> = resolved_uses.iter().map(|u| u.name.clone()).collect();
    assert!(
        !binding_names.is_empty(),
        "binding names: {:?}",
        binding_names
    );
    assert!(!use_names.is_empty(), "use names: {:?}", use_names);
}

// ────────────────────────────────────────────────────────────────
// Callsite-DataNode Backfill Test (Task 6 decision verification)
// ────────────────────────────────────────────────────────────────

/// Verify that callsite args_json[*].data_node_id is populated after
/// extraction — the backfill step 9a links ArgumentFact back to its
/// corresponding CallArg DataNode. This proves the callsite→dataflow
/// join is structurally sound.
#[test]
fn ts_callsite_args_link_to_datanode_callarg() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "app.ts",
            r#"import { helper } from './helper';
function main() {
    const result = helper(42, "text");
    return result;
}
"#,
        ),
        (
            "helper.ts",
            r#"export function helper(a: number, b: string): any {
    return { value: a, label: b };
}
"#,
        ),
    ];

    let (store, _stats) = index_files(files);
    let main_id = FileId::generate("app.ts");
    let _helper_id = FileId::generate("helper.ts");

    // Query callsites from main.ts (the call to helper is there)
    let main_callsites = store.find_callsites_by_file(&main_id).unwrap();
    // Find the callsite that has a callee symbol named "helper"
    let call_cs = main_callsites.iter().find(|cs| {
        cs.callee
            .and_then(|sym_id| store.find_symbol_by_id(&sym_id).ok().flatten())
            .map(|sym| sym.name == "helper")
            .unwrap_or(false)
    });
    assert!(
        call_cs.is_some(),
        "expected a callsite to helper() in main.ts. Found {} callsites: {:?}",
        main_callsites.len(),
        main_callsites
            .iter()
            .map(|cs| format!("callee_sym={:?} receiver={:?}", cs.callee, cs.receiver))
            .collect::<Vec<_>>()
    );
    let call_cs = call_cs.unwrap();
    assert!(
        !call_cs.args.is_empty(),
        "expected call arguments for helper() call"
    );

    let mut args_with_data_node = 0usize;
    for (i, arg) in call_cs.args.iter().enumerate() {
        if arg.data_node_id.is_some() {
            args_with_data_node += 1;
            // Sanity: the data_node_id should point to a real DataNode
            let dn = store.get_data_node(&arg.data_node_id.unwrap()).unwrap();
            assert!(
                dn.is_some(),
                "arg[{}].data_node_id → DataNode not found in DB",
                i
            );
            let dn = dn.unwrap();
            assert_eq!(
                dn.kind,
                atlas_engine::enums::DataNodeKind::CallArg,
                "arg[{}] links to kind={:?}, expected CallArg",
                i,
                dn.kind
            );
        }
    }
    assert!(
        args_with_data_node > 0,
        "expected at least one ArgumentFact with non-None data_node_id in callsite {:?}. \
         Got {}/{} args with data_node_id. Backfill step 9a may not be running.",
        call_cs.receiver,
        args_with_data_node,
        call_cs.args.len()
    );
}

// ────────────────────────────────────────────────────────────────
// Nested Call Arg-To-Param Edge Matching (Item 5 contract)
// ────────────────────────────────────────────────────────────────

/// Verify that nested calls produce correct ArgToParam edges.
/// Before callsite-grouped matching, ArgToParam used "most recent
/// preceding CallTarget" heuristic, which incorrectly linked args
/// in nested calls like `foo(bar(a), b)` — b would be linked to bar
/// instead of foo.
///
/// This contract test asserts that each CallArg links to the correct
/// CallTarget via its callsite_id group, not by source-position order.
#[test]
fn ts_nested_call_args_match_correct_target() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "nested.ts",
        r#"function bar(x: number): number {
    return x * 2;
}
function foo(a: number, b: number): number {
    return a + b;
}
function main(): number {
    const result = foo(bar(10), 20);
    return result;
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("nested.ts");

    // ── Find CallTarget nodes and their corresponding CallArg nodes ──
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let call_targets: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == atlas_engine::enums::DataNodeKind::CallTarget)
        .collect();
    assert!(
        call_targets.len() >= 2,
        "expected at least 2 CallTarget nodes (bar and foo), got {}",
        call_targets.len()
    );

    // Find bar's CallTarget node (the inner call)
    let _bar_target = call_targets
        .iter()
        .find(|ct| ct.access_path.as_deref() == Some("bar"))
        .expect("expected bar CallTarget node");
    // Find foo's CallTarget node (the outer call)
    let _foo_target = call_targets
        .iter()
        .find(|ct| ct.access_path.as_deref() == Some("foo"))
        .expect("expected foo CallTarget node");

    // ── Find CallArg nodes ──
    let call_args: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == atlas_engine::enums::DataNodeKind::CallArg)
        .collect();
    // Should have 3 args: 10 (→bar), result_of_bar_expression (→foo), 20 (→foo)
    // But tree-sitter may capture the full bar(10) expression as a single CallArg for foo
    assert!(
        call_args.len() >= 2,
        "expected at least 2 CallArg nodes, got {}",
        call_args.len()
    );

    // ── Verify ArgToCall edges go to correct target ──
    // Dataflow edges are per-DataNode (not symbol-level graph edges).
    // Query edges from each CallArg DataNode.
    let mut arg_to_call_edges = Vec::new();
    for ca in &call_args {
        if let Ok(edges) = store.find_dataflow_edges_by_source(&ca.id) {
            for e in &edges {
                if e.kind == atlas_engine::DataFlowKind::ArgToCall {
                    arg_to_call_edges.push((ca.id, e.clone()));
                }
            }
        }
    }
    assert!(
        !arg_to_call_edges.is_empty(),
        "expected ArgToCall dataflow edges for nested calls"
    );

    // For each ArgToCall edge, the source (CallArg) and target should
    // share the same callsite_id group.
    let mut mismatches = 0usize;
    for (src_id, edge) in &arg_to_call_edges {
        let src_node = nodes.iter().find(|n| n.id == *src_id);
        let tgt_node = nodes.iter().find(|n| n.id == edge.target);
        if let (Some(src), Some(tgt)) = (src_node, tgt_node) {
            if src.callsite_id != tgt.callsite_id && src.callsite_id.is_some() {
                mismatches += 1;
                eprintln!(
                    "MISMATCH: src={:?} (cs_id={:?}) -> tgt={:?} (cs_id={:?})",
                    src.access_path, src.callsite_id, tgt.access_path, tgt.callsite_id
                );
            }
        }
    }
    assert_eq!(
        mismatches,
        0,
        "ArgToCall edges should have matching callsite_id between source and target. \
         Found {} mismatches out of {} edges.",
        mismatches,
        arg_to_call_edges.len()
    );

    // Contract: the CallArg "20" (literal at line 8) should be linked
    // to foo's parameter, NOT bar's parameter.
    let arg_20 = call_args
        .iter()
        .find(|ca| {
            // "20" is the literal at line 8 (0-indexed line 7), column ~26
            ca.access_path.is_none() && ca.range.start_line == 7 && ca.range.start_byte > 95
        })
        .cloned();
    if let Some(ref arg_20) = arg_20 {
        let mut edges_to_20 = Vec::new();
        for ca in &call_args {
            if ca.id == arg_20.id {
                if let Ok(edges) = store.find_dataflow_edges_by_source(&ca.id) {
                    edges_to_20.extend(edges);
                }
            }
        }
        // arg 20 should have edges targeting foo-related nodes (not bar)
        for edge in &edges_to_20 {
            let tgt = nodes.iter().find(|n| n.id == edge.target);
            if let Some(tgt) = tgt {
                assert!(
                    tgt.access_path.as_deref() != Some("bar"),
                    "arg 20 should not link to bar, but found edge to {:?}",
                    tgt.access_path
                );
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Callsite-ID ↔ DataNode.callsite_id Join Contract (Fix 1 + Item 5)
// ────────────────────────────────────────────────────────────────

/// Verify that DataNode.callsite_id (provisional from_file_byte) and
/// Callsite.id (real from generate) share a consistent byte-range
/// join path.
///
/// Contract:
///   For any callsite cs: CS, any arg a: ArgumentFact with
///   a.data_node_id → dn: DataNode,
///   we assert:
///     dn.callsite_id == CallsiteId::from_file_byte(file_id, cs.range.start_byte)
///   AND
///     dn.kind == CallArg
///
/// This proves that the backfill step 9a correctly links callsites
/// to their corresponding CallArg DataNodes by call-expression byte
/// offset, and that the DataNode carries the correct callsite context.
#[test]
fn ts_datanode_callsite_id_join_matches_callsite_byte_range() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "calc.ts",
        r#"function add(a: number, b: number): number {
    return a + b;
}
function main(): number {
    const x = add(3, 4);
    const y = add(5, 6);
    return x + y;
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("calc.ts");

    let callsites = store.find_callsites_by_file(&file_id).unwrap();
    assert!(
        callsites.len() >= 2,
        "expected at least 2 callsites to add(), got {}",
        callsites.len()
    );

    for cs in &callsites {
        // Every arg with data_node_id must point to a CallArg DataNode
        // whose callsite_id matches the real Callsite.id (post-backfill rewrite).
        for (i, arg) in cs.args.iter().enumerate() {
            if let Some(dn_id) = &arg.data_node_id {
                let dn = store
                    .get_data_node(dn_id)
                    .unwrap()
                    .unwrap_or_else(|| panic!("arg[{}].data_node_id → DataNode not in DB", i));

                assert_eq!(
                    dn.kind,
                    atlas_engine::enums::DataNodeKind::CallArg,
                    "arg[{}] links to kind={:?}, expected CallArg",
                    i,
                    dn.kind
                );

                assert!(
                    dn.callsite_id.is_some(),
                    "arg[{}] DataNode should have callsite_id set (was None). \
                     This means the adapter didn't compute callsite_id from parent call_expression.",
                    i
                );

                // After P1 fixes (post-backfill rewrite), DataNode.callsite_id
                // is the real Callsite.id, NOT the provisional from_file_byte.
                assert_eq!(
                    dn.callsite_id.as_ref().unwrap(),
                    &cs.id,
                    "arg[{}] DataNode.callsite_id does not match callsite.id. \
                     This means the post-backfill rewrite (provisional→real \
                     callsite_id) did not correctly map this DataNode.",
                    i
                );
            }
        }
    }

    // Additionally, verify that there are no orphaned CallArg DataNodes
    // (missing from all callsites' args)
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    let call_arg_nodes: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == atlas_engine::enums::DataNodeKind::CallArg)
        .collect();
    assert!(
        !call_arg_nodes.is_empty(),
        "expected CallArg DataNodes in calc.ts"
    );

    // Collect all data_node_ids from all callsite args
    let linked_dn_ids: std::collections::HashSet<_> = callsites
        .iter()
        .flat_map(|cs| cs.args.iter().filter_map(|a| a.data_node_id.as_ref()))
        .collect();

    let unlinked: Vec<_> = call_arg_nodes
        .iter()
        .filter(|dn| !linked_dn_ids.contains(&dn.id))
        .collect();
    assert!(
        unlinked.is_empty(),
        "found {} CallArg DataNode(s) not linked to any callsite arg. \
         Backfill step 9a may have missed them. Nodes: {:?}",
        unlinked.len(),
        unlinked
            .iter()
            .map(|dn| format!("id={:?} range={:?}", dn.id, dn.range))
            .collect::<Vec<_>>()
    );
}

// ────────────────────────────────────────────────────────────────
// Cross-Function Use-Def Isolation Test (Fix 2 verification)
// ────────────────────────────────────────────────────────────────

/// Verify that dataflow edges do NOT cross function boundaries for
/// same-named local variables. The fix reordered resolve_dataflow_function_ids
/// before resolve_use_def so UseDefKey groups by (function_id, name).
/// Before the fix, function_id was None during use-def, causing cross-function
/// conflation.
#[test]
fn ts_dataflow_edges_stay_within_functions() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "fns.ts",
        r#"function foo() {
    let x = 1;
    let y = x + 1;
    return y;
}
function bar() {
    let x = 2;
    let z = x + 2;
    return z;
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("fns.ts");

    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    assert!(!nodes.is_empty(), "expected data nodes in fns.ts");

    // Build a quick lookup: DataNodeId → function_id
    let node_fn: std::collections::HashMap<_, _> =
        nodes.iter().map(|n| (n.id, n.function_id)).collect();

    // Collect all dataflow edges
    let mut cross_fn_edges: Vec<(atlas_engine::ids::DataNodeId, atlas_engine::ids::DataNodeId)> =
        vec![];
    for node in &nodes {
        if let Ok(edges) = store.find_dataflow_edges_by_source(&node.id) {
            for edge in &edges {
                let src_fn = node_fn.get(&edge.source);
                let tgt_fn = node_fn.get(&edge.target);
                if let (Some(&sf), Some(&tf)) = (src_fn, tgt_fn) {
                    if sf != tf {
                        cross_fn_edges.push((edge.source, edge.target));
                    }
                }
            }
        }
    }

    assert!(
        cross_fn_edges.is_empty(),
        "expected zero cross-function dataflow edges, but found {}: {:?}. \
         This indicates resolve_use_def may be running before resolve_dataflow_function_ids.",
        cross_fn_edges.len(),
        cross_fn_edges.iter().take(10).collect::<Vec<_>>()
    );

    // Also verify we DO have intra-function edges (the use-def should work within each fn)
    let intra_fn_edges: Vec<_> = nodes
        .iter()
        .flat_map(|n| {
            store
                .find_dataflow_edges_by_source(&n.id)
                .unwrap_or_default()
        })
        .filter(|e| {
            let sf = node_fn.get(&e.source).copied();
            let tf = node_fn.get(&e.target).copied();
            sf.is_some() && tf.is_some() && sf == tf
        })
        .collect();
    assert!(
        !intra_fn_edges.is_empty(),
        "expected some intra-function dataflow edges in fns.ts"
    );
}

// ────────────────────────────────────────────────────────────────
// MCP Server Integration Test (feature-gated)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "mcp")]
#[test]
fn mcp_tools_are_registered() {
    let _ = tracing_subscriber::fmt::try_init();

    let tools = atlas_mcp::make_all_tools();
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();

    // Core tools must be present
    assert!(
        tool_names.contains(&"atlas_status"),
        "atlas_status tool missing from MCP"
    );
    assert!(
        tool_names.contains(&"atlas_search"),
        "atlas_search tool missing from MCP"
    );
    assert!(
        tool_names.contains(&"atlas_symbol"),
        "atlas_symbol tool missing from MCP"
    );
    assert!(
        tool_names.contains(&"atlas_callgraph"),
        "atlas_callgraph tool missing from MCP"
    );
    assert!(
        tool_names.contains(&"atlas_path"),
        "atlas_path tool missing from MCP"
    );
    assert!(
        tool_names.contains(&"atlas_explore"),
        "atlas_explore tool missing from MCP"
    );
    assert!(
        tool_names.contains(&"atlas_context"),
        "atlas_context tool missing from MCP"
    );
}
