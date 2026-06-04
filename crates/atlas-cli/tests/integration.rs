//! Integration tests for Atlas — end-to-end multi-file pipelines.
//!
//! These tests create temporary directories, write source files, run the
//! full extraction→storage→resolution→graph pipeline, and verify results.
//!
//! Run with default features:  `cargo test --test integration`
//! Run with all languages:    `cargo test --test integration --features all-languages,mcp,sync`

use atlas_engine::GraphBuilder;
use atlas_engine::Store;
use atlas_engine::enums::{EdgeKind, Language};
use atlas_engine::extract_file;
use atlas_engine::ids::FileId;
use atlas_engine::{ReferenceResolver, ResolutionStats};
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
            .unwrap_or_else(|| panic!("no language detected for {rel_path}"));
        let frontend = atlas_engine::create_frontend(lang)
            .unwrap_or_else(|| panic!("no frontend for {rel_path} (lang={lang:?})"));
        let file_id = FileId::generate(rel_path);
        let facts = extract_file(&frontend, file_id, &PathBuf::from(rel_path), content, "abc")
            .unwrap_or_else(|e| panic!("extract {rel_path} failed: {e:?}"));
        store
            .insert_file_facts(&facts)
            .unwrap_or_else(|e| panic!("insert {rel_path} failed: {e:?}"));
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
        "expected 'process' in callers of 'helper', got {caller_names:?}"
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
        "Calculator not found in lib symbols: {names:?}"
    );
    assert!(
        names.contains(&"create_calculator".to_string()),
        "create_calculator not found in lib symbols: {names:?}"
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
        "expected at least 2 different edge kinds, got {all_edge_kinds:?}"
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
        "Greeter class not found: {names:?}"
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
                if edge.location.start_byte > 0
                    && edge.location.end_byte > 0
                    && (edge.location.start_column > 0 || edge.location.end_column > 0)
                {
                    edges_with_column = true;
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
        "binding names: {binding_names:?}"
    );
    assert!(!use_names.is_empty(), "use names: {use_names:?}");
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
                "arg[{i}].data_node_id → DataNode not found in DB"
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
    if let Some(arg_20) = arg_20 {
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
                    .unwrap_or_else(|| panic!("arg[{i}].data_node_id → DataNode not in DB"));

                assert_eq!(
                    dn.kind,
                    atlas_engine::enums::DataNodeKind::CallArg,
                    "arg[{}] links to kind={:?}, expected CallArg",
                    i,
                    dn.kind
                );

                assert!(
                    dn.callsite_id.is_some(),
                    "arg[{i}] DataNode should have callsite_id set (was None). \
                     This means the adapter didn't compute callsite_id from parent call_expression."
                );

                // After P1 fixes (post-backfill rewrite), DataNode.callsite_id
                // is the real Callsite.id, NOT the provisional from_file_byte.
                assert_eq!(
                    dn.callsite_id.as_ref().unwrap(),
                    &cs.id,
                    "arg[{i}] DataNode.callsite_id does not match callsite.id. \
                     This means the post-backfill rewrite (provisional→real \
                     callsite_id) did not correctly map this DataNode."
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

    assert_eq!(
        tool_names.len(),
        18,
        "expected exactly 18 MCP tools, got {}",
        tool_names.len()
    );
    assert!(tool_names.contains(&"project"));
    assert!(tool_names.contains(&"index"));
    assert!(tool_names.contains(&"search"));
    assert!(tool_names.contains(&"symbol"));
    assert!(tool_names.contains(&"calls"));
    assert!(tool_names.contains(&"explore"));
    assert!(tool_names.contains(&"path"));
    assert!(tool_names.contains(&"impact"));
    assert!(tool_names.contains(&"file_dependencies"));
    assert!(tool_names.contains(&"trace"));
    assert!(tool_names.contains(&"lifecycle"));
    assert!(tool_names.contains(&"branch_diff"));
    assert!(tool_names.contains(&"fp_dispatches"));
    assert!(tool_names.contains(&"domain_rules"));
    assert!(tool_names.contains(&"tasks"));
    assert!(tool_names.contains(&"task_status"));
    assert!(tool_names.contains(&"wait_for_task"));
    assert!(tool_names.contains(&"resume_task"));
}

/// Verify that DataNodes and dataflow_edges are cascade-deleted
/// when the owning file is removed.  SQLite FOREIGN KEY ON DELETE CASCADE
/// handles this automatically, but this test guards against regression.
#[test]
fn ts_delete_file_cascades_dataflow() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "cascade_test.ts",
        r#"
function add(a: number, b: number): number {
    let sum = a + b;
    return sum;
}
let result = add(1, 2);
"#,
    )];

    let (store, _) = index_files(files);
    let file_id = FileId::generate("cascade_test.ts");

    // Verify dataflow records exist
    let nodes = store.find_data_nodes_by_file(&file_id).unwrap();
    assert!(!nodes.is_empty(), "DataNodes should exist after index");
    let edges = store.find_dataflow_edges_by_file(&file_id).unwrap();
    assert!(!edges.is_empty(), "DataFlowEdges should exist after index");

    // Delete
    store.delete_file_data(&file_id).unwrap();

    // Verify cascade
    let nodes_after = store.find_data_nodes_by_file(&file_id).unwrap();
    assert!(
        nodes_after.is_empty(),
        "DataNodes must be cascade-deleted after file delete"
    );
    let edges_after = store.find_dataflow_edges_by_file(&file_id).unwrap();
    assert!(
        edges_after.is_empty(),
        "DataFlowEdges must be cascade-deleted after file delete"
    );
}

/// Barrel re-export chain: main.ts imports `{ greet }` from barrel/index.ts,
/// which re-exports via `export * from './lib'` where greet is defined.
/// Verifies that resolution follows the chain to the actual definition.
#[test]
fn ts_barrel_reexport_chain_resolves_to_source() {
    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "lib/helper.ts",
            r#"export function greet(name: string): string {
    return "Hello, " + name;
}
"#,
        ),
        (
            "src/index.ts",
            r#"export * from '../lib/helper';
"#,
        ),
        (
            "src/app.ts",
            r#"import { greet } from './index';

function main() {
    console.log(greet("World"));
}
"#,
        ),
    ];

    let (store, stats) = index_files(files);

    // The import from './index' should resolve through the barrel's
    // `export * from '../lib/helper'` to lib/helper.ts::greet.
    let lib_file_id = FileId::generate("lib/helper.ts");
    let lib_syms = store.find_symbols_by_file(&lib_file_id).unwrap();
    assert!(
        lib_syms.iter().any(|s| s.name == "greet"),
        "greet should be defined in lib/helper.ts"
    );

    // Verify edges were created from resolution
    let all_edges = store.get_all_edges().unwrap();
    assert!(
        !all_edges.is_empty(),
        "resolution should produce edges (including through barrel chain)"
    );

    // The greet symbol from lib/helper.ts should be a target of some edge
    let greet_sym = lib_syms.iter().find(|s| s.name == "greet").unwrap();
    let has_reference = all_edges
        .iter()
        .any(|e| e.target == greet_sym.id && e.kind == EdgeKind::Calls);
    assert!(
        has_reference,
        "greet from lib/helper.ts should be referenced via barrel re-export \
         (no edge found targeting greet). edges built: {}",
        stats.edges_built,
    );
}

// ────────────────────────────────────────────────────────────────
// Impact analysis end-to-end integration test
// ────────────────────────────────────────────────────────────────

#[test]
fn ts_impact_analysis_end_to_end() {
    use atlas_engine::GraphEngine;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[
        (
            "lib.ts",
            r#"export function helper(x: number): number {
    return x * 2;
}
export class Calculator {
    add(a: number, b: number): number {
        return a + b;
    }
}
"#,
        ),
        (
            "app.ts",
            r#"import { helper, Calculator } from './lib';

function compute(): number {
    const calc = new Calculator();
    const doubled = helper(5);
    return calc.add(doubled, 1);
}
"#,
        ),
    ];
    let (store, _stats) = index_files(files);
    let engine = GraphEngine::from_store(&store, 0.0).unwrap();

    // Find compute function
    let app_id = FileId::generate("app.ts");
    let app_syms = store.find_symbols_by_file(&app_id).unwrap();
    let compute_sym = app_syms
        .iter()
        .find(|s| s.name == "compute")
        .expect("compute not found");

    // Run impact analysis on compute
    let sub = engine.impact(&compute_sym.id, 3);
    assert!(
        !sub.node_indices.is_empty(),
        "impact should find reachable nodes"
    );

    // Resolve reached node IDs
    let reached_ids: Vec<_> = sub
        .node_indices
        .iter()
        .map(|ix| engine.snapshot().node(*ix).symbol_id)
        .collect();
    let reached_syms: Vec<_> = reached_ids
        .iter()
        .filter_map(|id| store.find_symbol_by_id(id).ok())
        .flatten()
        .collect();
    let reached_names: Vec<_> = reached_syms.iter().map(|s| s.name.as_str()).collect();

    // Should reach at least compute (self), helper (Calls), add (Calls via calc.add())
    assert!(
        reached_names.contains(&"helper"),
        "impact should reach helper via Calls, got {reached_names:?}"
    );
    assert!(
        reached_names.contains(&"add"),
        "impact should reach Calculator.add via Calls, got {reached_names:?}"
    );
    assert!(
        reached_names.len() >= 3,
        "impact should reach at least 3 nodes, got {reached_names:?}"
    );
}

// ────────────────────────────────────────────────────────────────
// Python `with` lifecycle test (Part 1e)
// ────────────────────────────────────────────────────────────────

#[test]
fn test_python_with_lifecycle() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::{ConsumptionStyle, PlaceRef, SemanticEffectKind};

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "with_lifecycle.py",
        include_str!("fixtures/python/with_lifecycle.py"),
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("with_lifecycle.py");

    // Find the read_config function
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let read_config_sym = syms
        .iter()
        .find(|s| s.name == "read_config")
        .expect("read_config function not found");

    // Load CFG
    let cfg_nodes = store
        .find_cfg_nodes_by_function(&read_config_sym.id)
        .unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

    let cfg_edges = store
        .find_cfg_edges_by_function(&read_config_sym.id)
        .unwrap();

    // Verify BlockExit node exists
    let has_block_exit = cfg_nodes
        .iter()
        .any(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit);
    assert!(
        has_block_exit,
        "CFG should contain a BlockExit node for with_statement"
    );

    // Build CfgGraph
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store
        .find_data_nodes_by_function(&read_config_sym.id)
        .unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::Python);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    // Assert that open() produces an Alloc effect
    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();
    let has_alloc_for_open = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "open"),
    );
    assert!(has_alloc_for_open, "Expected an Alloc effect for open()");

    // Assert that a Free effect exists at a BlockExit node
    let has_free_at_block_exit = cfg_nodes.iter().any(|n| {
        n.kind == atlas_engine::CfgNodeKind::BlockExit
            && composition.node_effects.get(&n.id).map_or(false, |effs| {
                effs.iter()
                    .any(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }))
            })
    });
    assert!(
        has_free_at_block_exit,
        "Expected a Free effect at BlockExit node"
    );

    // Assert the Free has ConsumptionStyle::ContextManaged
    let context_managed_free = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Free { .. })
            && eff.consumption_style == Some(ConsumptionStyle::ContextManaged)
    });
    assert!(
        context_managed_free,
        "Expected a Free effect with ContextManaged consumption style"
    );
}

// ────────────────────────────────────────────────────────────────
// React useEffect cleanup return test (Part 2e)
// ────────────────────────────────────────────────────────────────

#[test]
fn test_ts_react_cleanup() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::{ConsumptionStyle, SemanticEffectKind};

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "react_effect_cleanup.tsx",
        include_str!("fixtures/typescript/react_effect_cleanup.tsx"),
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("react_effect_cleanup.tsx");

    // Collect ALL function symbols in the file (arrow functions get
    // separate symbols with independent CFGs in Full mode).
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let fn_syms: Vec<_> = syms
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                atlas_engine::SymbolKind::Function
                    | atlas_engine::SymbolKind::Method
                    | atlas_engine::SymbolKind::Constructor
            )
        })
        .collect();

    assert!(
        !fn_syms.is_empty(),
        "Expected at least one function symbol in the file"
    );

    // Run compose_effects for every function, collecting all effects
    // across all scopes.  With the per-node CallContext::ReactEffectCleanup
    // gating the Deferred marking only applies inside the cleanup arrow
    // body — not across the entire Timer function.
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::TypeScript);
    let mut all_effects: Vec<atlas_engine::effects::SemanticEffect> = Vec::new();

    for sym in &fn_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .unwrap_or_default();
        if cfg_nodes.is_empty() {
            continue;
        }
        let cfg_edges = store
            .find_cfg_edges_by_function(&sym.id)
            .unwrap_or_default();
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");
        let data_nodes = store
            .find_data_nodes_by_function(&sym.id)
            .unwrap_or_default();
        let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
            vec![]
        } else {
            let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&all_ids)
                .unwrap_or_default()
        };

        if cfg_graph.nodes.is_empty() {
            continue;
        }

        let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);
        all_effects.extend(composition.node_effects.values().flatten().cloned());
    }

    // Assert useEffect → Alloc (MaybeOwned at 0.6 confidence)
    let use_effect_alloc = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "useEffect")
    });
    assert!(use_effect_alloc, "Expected an Alloc effect for useEffect");

    // Assert setInterval → Alloc (NewOwned)
    let set_interval_alloc = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "setInterval")
    });
    assert!(
        set_interval_alloc,
        "Expected an Alloc effect for setInterval"
    );

    // Assert clearInterval → Free (Deferred) — only inside the cleanup
    // arrow body, which has its own CFG with ReactEffectCleanup context.
    let clear_interval_free = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Free { callee, .. } if callee == "clearInterval")
            && eff.consumption_style == Some(ConsumptionStyle::Deferred)
    });
    assert!(
        clear_interval_free,
        "Expected a Deferred Free effect for clearInterval (in cleanup arrow scope)"
    );

    // Verify ConsumptionStyle::Deferred is set on at least one Free
    let has_deferred_free = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Free { .. })
            && eff.consumption_style == Some(ConsumptionStyle::Deferred)
    });
    assert!(
        has_deferred_free,
        "Expected at least one Free effect with Deferred consumption style"
    );
}

// ────────────────────────────────────────────────────────────────
// Go Goroutine Escape Test (Part C)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "go")]
#[test]
fn test_go_goroutine_escape() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::{EscapeTarget, SemanticEffectKind};

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "goroutine.go",
        r#"package main

import "os"

func main() {
	go func() {
		f, _ := os.Open("file.txt")
		f.Close()
	}()
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("goroutine.go");

    // Find the main function
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let main_sym = syms
        .iter()
        .find(|s| s.name == "main" && s.kind.as_str() == "function")
        .expect("main function not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&main_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

    let cfg_edges = store.find_cfg_edges_by_function(&main_sym.id).unwrap();

    // Build CfgGraph
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&main_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with Go OwnershipContract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::Go);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert that an Escape effect with EscapeTarget::Thread is produced
    // (the goroutine context causes resource escape)
    let has_thread_escape = all_effects.iter().any(|eff| {
        matches!(
            &eff.kind,
            SemanticEffectKind::Escape {
                to: EscapeTarget::Thread,
                ..
            }
        )
    });
    assert!(
        has_thread_escape,
        "Expected an Escape effect with EscapeTarget::Thread. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>(),
    );
}

// ────────────────────────────────────────────────────────────────
// Go `compose_effects` produces real effects (P0 regression)
// ────────────────────────────────────────────────────────────────

/// Regression: Go CalleeMatcher rules previously never fired because
/// normalizer stored terminal field_identifier text instead of the full
/// selector_expression qualified name.  This test verifies that actual Go
/// code with `os.Open` + `defer f.Close` produces real Alloc and Free
/// effects when run through the full compose_effects pipeline.
#[cfg(feature = "go")]
#[test]
fn test_go_composition_produces_real_effects() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "real_effects.go",
        r#"package main

import "os"

func main() {
	f, _ := os.Open("file.txt")
	defer f.Close()
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("real_effects.go");

    // Find the main function
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let main_sym = syms
        .iter()
        .find(|s| s.name == "main" && s.kind.as_str() == "function")
        .expect("main function not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&main_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

    let cfg_edges = store.find_cfg_edges_by_function(&main_sym.id).unwrap();

    // Build CfgGraph
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&main_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with Go contract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::Go);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert there is at least one Alloc effect (from os.Open)
    let has_alloc = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee.contains("Open"))
    });
    assert!(
        has_alloc,
        "Expected at least one Alloc effect from os.Open. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>(),
    );

    // Assert there is at least one Free effect (from f.Close via defer)
    let has_free = all_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("Close"))
    });
    assert!(
        has_free,
        "Expected at least one Free effect from f.Close. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>(),
    );
}

// ────────────────────────────────────────────────────────────────
// Rust Scope Exit Test (Drop at Exit)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "rust")]
#[test]
fn test_rust_scope_exit() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::scope_exit::run_scope_exit_pass;
    use atlas_engine::effects::{PlaceRef, SemanticEffect, SemanticEffectKind};
    use atlas_engine::ids::EffectId;
    use std::collections::HashMap;

    let _ = tracing_subscriber::fmt::try_init();
    // Use a simple Rust source for CFG construction.
    // The scope_exit_pass is tested here with manually constructed effects,
    // since the compose_effects pipeline's callee classification depends on
    // the dataflow query capturing specific call patterns (e.g., path-based
    // calls like Box::new are not yet captured by the Rust dataflow query).
    let files = &[(
        "scope_exit.rs",
        r#"fn main() {
    let x = alloc(42);
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("scope_exit.rs");

    // Find the main function
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let main_sym = syms
        .iter()
        .find(|s| s.name == "main" && s.kind.as_str() == "function")
        .expect("main function not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&main_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

    let cfg_edges = store.find_cfg_edges_by_function(&main_sym.id).unwrap();

    // Build CfgGraph
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Find a Statement node and the Exit node
    let stmt_node = cfg_nodes
        .iter()
        .find(|n| n.kind == atlas_engine::CfgNodeKind::Statement)
        .expect("Statement node should exist");
    let exit_node = cfg_nodes
        .iter()
        .find(|n| n.kind == atlas_engine::CfgNodeKind::Exit)
        .expect("Exit node should exist");

    // Manually construct an Alloc effect for an unfreed local
    let place = PlaceRef::Local {
        name: "x".to_string(),
    };
    let alloc_kind = SemanticEffectKind::Alloc {
        target: place.clone(),
        callee: "Box::new".to_string(),
    };
    let alloc_effect = SemanticEffect {
        id: EffectId::generate(&stmt_node.id, 0, "Alloc"),
        cfg_node_id: stmt_node.id,
        order: 0,
        kind: alloc_kind,
        confidence: 0.85,
        consumption_style: None,
        description: None,
        eligible_for_implicit_cleanup: None,
    };
    let mut effects: HashMap<atlas_engine::ids::CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
    effects.insert(stmt_node.id, vec![alloc_effect]);

    // Run scope_exit_pass — should add a Free at Exit for the unfreed alloc
    run_scope_exit_pass(&mut effects, &cfg_graph);

    // Verify the Exit node has a scope-exit Free
    let exit_effects = effects.get(&exit_node.id);
    assert!(
        exit_effects.is_some(),
        "Exit node should have scope-exit effects"
    );
    let exit_effects = exit_effects.unwrap();
    let has_scope_exit_free = exit_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<scope-exit>"))
    });
    assert!(
        has_scope_exit_free,
        "Expected a scope-exit Free effect at Exit node for unfreed allocation. \
         Exit node effects: {:?}",
        exit_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[cfg(feature = "java")]
#[test]
fn test_java_try_with_lifecycle() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::scope_exit::run_scope_exit_pass;
    use atlas_engine::effects::{ConsumptionStyle, PlaceRef, SemanticEffect, SemanticEffectKind};
    use atlas_engine::ids::EffectId;
    use std::collections::HashMap;

    let _ = tracing_subscriber::fmt::try_init();

    // Index a Java source file with try-with-resources
    let files = &[(
        "try_resource.java",
        r#"import java.io.*;

class ResourceTest {
    void readFile() throws IOException {
        try (FileInputStream fis = new FileInputStream("data.txt")) {
            byte[] buf = new byte[1024];
            fis.read(buf);
        }
    }
}
"#,
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("try_resource.java");

    // Find the readFile method
    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let method_sym = syms
        .iter()
        .find(|s| s.name == "readFile" && s.kind.as_str() == "method")
        .expect("readFile method not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&method_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

    // Verify there is a BlockExit node
    let has_block_exit = cfg_nodes
        .iter()
        .any(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit);
    assert!(
        has_block_exit,
        "CFG should have a BlockExit node for try-with-resources"
    );

    let cfg_edges = store.find_cfg_edges_by_function(&method_sym.id).unwrap();

    // Build CfgGraph
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Find a Statement node with JavaTryWith context and the BlockExit node
    let stmt_node = cfg_nodes.iter().find(|n| {
        n.kind == atlas_engine::CfgNodeKind::Statement
            && n.call_context == atlas_engine::enums::CallContext::JavaTryWith
    });
    assert!(
        stmt_node.is_some(),
        "Should find a Statement with JavaTryWith context"
    );
    let stmt_node = stmt_node.unwrap();

    let be_node = cfg_nodes
        .iter()
        .find(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit)
        .expect("BlockExit node should exist");

    let exit_node = cfg_nodes
        .iter()
        .find(|n| n.kind == atlas_engine::CfgNodeKind::Exit)
        .expect("Exit node should exist");

    // Manually construct an Alloc effect for the resource.
    // NOTE: compose_effects would be preferred here but tree-sitter-java parses
    // `new FileInputStream(...)` as `object_creation_expression`, not
    // `method_invocation`, so the DataNode extraction doesn't produce
    // CallTarget nodes for constructor calls in try-with-resources.
    // Manual Alloc construction is retained as a fallback until Java DataNode
    // extraction adds support for object_creation_expression CallTarget capture.
    let place = PlaceRef::Local {
        name: "fis".to_string(),
    };
    let alloc_kind = SemanticEffectKind::Alloc {
        target: place.clone(),
        callee: "newInputStream".to_string(),
    };
    let alloc_effect = SemanticEffect {
        id: EffectId::generate(&stmt_node.id, 0, "Alloc"),
        cfg_node_id: stmt_node.id,
        order: 0,
        kind: alloc_kind,
        confidence: 0.85,
        consumption_style: None,
        description: None,
        eligible_for_implicit_cleanup: None,
    };
    let mut effects: HashMap<atlas_engine::ids::CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
    effects.insert(stmt_node.id, vec![alloc_effect]);

    // Run scope_exit_pass — should add a Free at BlockExit for the JavaTryWith alloc
    run_scope_exit_pass(&mut effects, &cfg_graph);

    // Verify the BlockExit node has a Free with ContextManaged style
    let be_effects = effects.get(&be_node.id);
    assert!(
        be_effects.is_some(),
        "BlockExit node should have scope-exit effects for JavaTryWith"
    );
    let be_effects = be_effects.unwrap();
    let has_block_exit_free = be_effects.iter().any(|eff| {
        matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
            if callee.contains("<block-exit>"))
    });
    assert!(
        has_block_exit_free,
        "Expected a block-exit Free effect at BlockExit for JavaTryWith alloc"
    );

    // Verify the ConsumptionStyle is ContextManaged
    let block_exit_free = be_effects
        .iter()
        .find(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
    assert!(block_exit_free.is_some(), "Should have a Free effect");
    assert_eq!(
        block_exit_free.unwrap().consumption_style,
        Some(ConsumptionStyle::ContextManaged),
        "JavaTryWith Free should have ContextManaged consumption style"
    );

    // Exit node should NOT have this Free
    if let Some(exit_effects) = effects.get(&exit_node.id) {
        let has_scope_exit_free = exit_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                if callee.contains("<scope-exit>") || callee.contains("<block-exit>"))
        });
        assert!(
            !has_scope_exit_free,
            "Exit node should NOT have the scope-exit Free when BlockExit was reached for JavaTryWith"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// C# using statement lifecycle tests
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "csharp")]
mod csharp_tests {
    use super::*;
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::resource_ops::ResourceOpConfig;
    use atlas_engine::analysis::scope_exit::run_scope_exit_pass;
    use atlas_engine::effects::{ConsumptionStyle, PlaceRef, SemanticEffect, SemanticEffectKind};
    use atlas_engine::ids::EffectId;
    use std::collections::HashMap;

    #[test]
    fn test_csharp_using_lifecycle() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "using_dispose.cs",
            r#"using System;
using System.IO;

class ResourceDemo
{
    void ReadFile()
    {
        using (var stream = new FileStream("data.txt", FileMode.Open))
        {
            byte[] buffer = new byte[1024];
            stream.Read(buffer, 0, buffer.Length);
        }
    }
}
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("using_dispose.cs");

        // Find the ReadFile method
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let method_sym = syms
            .iter()
            .find(|s| s.name == "ReadFile" && s.kind.as_str() == "method")
            .expect("ReadFile method not found");

        // Load CFG
        let cfg_nodes = store.find_cfg_nodes_by_function(&method_sym.id).unwrap();
        assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

        // Verify there is a BlockExit node
        let has_block_exit = cfg_nodes
            .iter()
            .any(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit);
        assert!(
            has_block_exit,
            "CFG should have a BlockExit node for using statement"
        );

        let cfg_edges = store.find_cfg_edges_by_function(&method_sym.id).unwrap();

        // Build CfgGraph
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

        // Find a Statement node with CSharpUsing context and the BlockExit node
        let stmt_node = cfg_nodes.iter().find(|n| {
            n.kind == atlas_engine::CfgNodeKind::Statement
                && n.call_context == atlas_engine::enums::CallContext::CSharpUsing
        });
        assert!(
            stmt_node.is_some(),
            "Should find a Statement with CSharpUsing context"
        );
        let stmt_node = stmt_node.unwrap();

        let be_node = cfg_nodes
            .iter()
            .find(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit)
            .expect("BlockExit node should exist");

        let exit_node = cfg_nodes
            .iter()
            .find(|n| n.kind == atlas_engine::CfgNodeKind::Exit)
            .expect("Exit node should exist");

        // Manually construct an Alloc effect for the resource.
        // TODO: Replace with compose_effects once C# DataNode extraction produces
        // CallTarget nodes for object_creation_expression patterns (similar to
        // the Java limitation — tree-sitter-c-sharp also parses `new FileStream(...)`
        // as object_creation_expression, not method_invocation).
        let place = PlaceRef::Local {
            name: "stream".to_string(),
        };
        let alloc_kind = SemanticEffectKind::Alloc {
            target: place.clone(),
            callee: "new FileStream".to_string(),
        };
        let alloc_effect = SemanticEffect {
            id: EffectId::generate(&stmt_node.id, 0, "Alloc"),
            cfg_node_id: stmt_node.id,
            order: 0,
            kind: alloc_kind,
            confidence: 0.85,
            consumption_style: None,
            description: None,
            eligible_for_implicit_cleanup: None,
        };
        let mut effects: HashMap<atlas_engine::ids::CfgNodeId, Vec<SemanticEffect>> =
            HashMap::new();
        effects.insert(stmt_node.id, vec![alloc_effect]);

        // Run scope_exit_pass — should add a Free at BlockExit for the CSharpUsing alloc
        run_scope_exit_pass(&mut effects, &cfg_graph);

        // Verify the BlockExit node has a Free with ContextManaged style
        let be_effects = effects.get(&be_node.id);
        assert!(
            be_effects.is_some(),
            "BlockExit node should have scope-exit effects for CSharpUsing"
        );
        let be_effects = be_effects.unwrap();
        let has_block_exit_free = be_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                if callee.contains("<block-exit>"))
        });
        assert!(
            has_block_exit_free,
            "Expected a block-exit Free effect at BlockExit for CSharpUsing alloc"
        );

        // Verify the ConsumptionStyle is ContextManaged
        let block_exit_free = be_effects
            .iter()
            .find(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
        assert!(block_exit_free.is_some(), "Should have a Free effect");
        assert_eq!(
            block_exit_free.unwrap().consumption_style,
            Some(ConsumptionStyle::ContextManaged),
            "CSharpUsing Free should have ContextManaged consumption style"
        );

        // Exit node should NOT have this Free
        if let Some(exit_effects) = effects.get(&exit_node.id) {
            let has_scope_exit_free = exit_effects.iter().any(|eff| {
                matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                    if callee.contains("<scope-exit>") || callee.contains("<block-exit>"))
            });
            assert!(
                !has_scope_exit_free,
                "Exit node should NOT have the scope-exit Free when BlockExit was reached for CSharpUsing"
            );
        }
    }

    #[test]
    fn test_csharp_resource_patterns() {
        let config = ResourceOpConfig::default_for(Language::CSharp);
        // Producers
        assert!(config.is_producer("File.Open"));
        assert!(config.is_producer("new FileStream"));
        assert!(config.is_producer("SqlConnection"));
        assert!(config.is_producer("OpenConnection"));
        assert!(config.is_producer("OpenStream"));
        // Consumers
        assert_eq!(config.is_consumer("conn.Dispose"), Some(0));
        assert_eq!(config.is_consumer("stream.Close"), Some(0));
        // Non-patterns
        assert!(!config.is_producer("free"));
        assert_eq!(config.is_consumer("open"), None);
    }

    #[test]
    fn test_csharp_using_multiple_resources() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "using_multi.cs",
            r#"using System;
using System.IO;

class MultiDemo
{
    void ProcessFiles()
    {
        using (var input = new FileStream("in.txt", FileMode.Open),
                    var output = new FileStream("out.txt", FileMode.Create))
        {
            byte[] buffer = new byte[1024];
            input.Read(buffer, 0, buffer.Length);
            output.Write(buffer, 0, buffer.Length);
        }
    }
}
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("using_multi.cs");

        // Find the ProcessFiles method
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let method_sym = syms
            .iter()
            .find(|s| s.name == "ProcessFiles" && s.kind.as_str() == "method")
            .expect("ProcessFiles method not found");

        // Load CFG
        let cfg_nodes = store.find_cfg_nodes_by_function(&method_sym.id).unwrap();
        assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

        // Verify there is a BlockExit node
        let has_block_exit = cfg_nodes
            .iter()
            .any(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit);
        assert!(
            has_block_exit,
            "CFG should have a BlockExit node for using statement"
        );

        let cfg_edges = store.find_cfg_edges_by_function(&method_sym.id).unwrap();

        use atlas_engine::analysis::cfg_graph::CfgGraph;
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

        // Find Statement nodes with CSharpUsing context
        let using_stmts: Vec<_> = cfg_nodes
            .iter()
            .filter(|n| {
                n.kind == atlas_engine::CfgNodeKind::Statement
                    && n.call_context == atlas_engine::enums::CallContext::CSharpUsing
            })
            .collect();
        assert!(
            !using_stmts.is_empty(),
            "Should find at least one Statement with CSharpUsing context"
        );

        // Find the BlockExit node
        let be_node = cfg_nodes
            .iter()
            .find(|n| n.kind == atlas_engine::CfgNodeKind::BlockExit)
            .expect("BlockExit node should exist");

        // Manually construct Alloc effects for both resources on the same statement node
        use atlas_engine::analysis::scope_exit::run_scope_exit_pass;
        use atlas_engine::effects::{
            ConsumptionStyle, PlaceRef, SemanticEffect, SemanticEffectKind,
        };
        use atlas_engine::ids::EffectId;
        use std::collections::HashMap;

        let mut effects: HashMap<atlas_engine::ids::CfgNodeId, Vec<SemanticEffect>> =
            HashMap::new();

        // Both `using` resources are on the same statement (multi-declaration using)
        let stmt = &using_stmts[0];
        let stmt_effects = effects.entry(stmt.id).or_default();

        let alloc_input = SemanticEffect {
            id: EffectId::generate(&stmt.id, 0, "Alloc"),
            cfg_node_id: stmt.id,
            order: 0,
            kind: SemanticEffectKind::Alloc {
                target: PlaceRef::Local {
                    name: "input".to_string(),
                },
                callee: "new FileStream".to_string(),
            },
            confidence: 0.85,
            consumption_style: None,
            description: None,
            eligible_for_implicit_cleanup: None,
        };
        stmt_effects.push(alloc_input);

        let alloc_output = SemanticEffect {
            id: EffectId::generate(&stmt.id, 1, "Alloc"),
            cfg_node_id: stmt.id,
            order: 1,
            kind: SemanticEffectKind::Alloc {
                target: PlaceRef::Local {
                    name: "output".to_string(),
                },
                callee: "new FileStream".to_string(),
            },
            confidence: 0.85,
            consumption_style: None,
            description: None,
            eligible_for_implicit_cleanup: None,
        };
        stmt_effects.push(alloc_output);

        // Run scope_exit_pass
        run_scope_exit_pass(&mut effects, &cfg_graph);

        // Both resources should get scoped exit Free at the same BlockExit
        let be_effects = effects.get(&be_node.id);
        assert!(
            be_effects.is_some(),
            "BlockExit node should have scope-exit effects for multi-resource using"
        );
        let be_effects = be_effects.unwrap();

        let free_count = be_effects
            .iter()
            .filter(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }))
            .count();
        assert!(
            free_count >= 2,
            "Expected >= 2 Free effects at BlockExit for multiple using resources, got {}",
            free_count
        );

        // Verify all Free effects have ContextManaged style
        for eff in be_effects {
            if matches!(&eff.kind, SemanticEffectKind::Free { .. }) {
                assert_eq!(
                    eff.consumption_style,
                    Some(atlas_engine::effects::ConsumptionStyle::ContextManaged),
                    "CSharpUsing Free should have ContextManaged consumption style"
                );
            }
        }

        // Exit node should NOT have these Frees
        if let Some(exit_node) = cfg_nodes
            .iter()
            .find(|n| n.kind == atlas_engine::CfgNodeKind::Exit)
        {
            if let Some(exit_effects) = effects.get(&exit_node.id) {
                let has_scope_exit_free = exit_effects.iter().any(|eff| {
                    matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                        if callee.contains("<scope-exit>") || callee.contains("<block-exit>"))
                });
                assert!(
                    !has_scope_exit_free,
                    "Exit node should NOT have scope/block-exit Frees when BlockExit was reached"
                );
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Kotlin .use lifecycle tests
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "kotlin")]
mod kotlin_tests {
    use super::*;
    use atlas_engine::analysis::resource_ops::ResourceOpConfig;

    #[test]
    fn test_kotlin_use_lifecycle() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "use_resource.kt",
            r#"import java.io.File

fun readFile() {
    val file = File("data.txt")
    file.bufferedReader().use { reader ->
        val line = reader.readLine()
        println(line)
    }
}
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("use_resource.kt");

        // Find the readFile function
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let func_sym = syms
            .iter()
            .find(|s| s.name == "readFile" && s.kind.as_str() == "function")
            .expect("readFile function not found");

        // Verify that CFG nodes were extracted
        let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
        assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

        let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();

        use atlas_engine::analysis::cfg_graph::CfgGraph;
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

        // Load DataFlow
        let data_nodes = store.find_data_nodes_by_function(&func_sym.id).unwrap();
        let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
            vec![]
        } else {
            let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&all_ids)
                .unwrap_or_default()
        };

        // Run compose_effects with Kotlin ResourceOpConfig
        use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
        use atlas_engine::effects::SemanticEffectKind;
        let contract = ResourceOpConfig::default_for(atlas_engine::Language::Kotlin);
        let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

        let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

        use atlas_engine::enums::CallContext;
        use atlas_engine::enums::CfgNodeKind;

        // Verify BlockExit node exists (emitted for Kotlin .use {} blocks)
        let has_block_exit = cfg_graph
            .nodes
            .values()
            .any(|n| n.kind == CfgNodeKind::BlockExit);
        assert!(
            has_block_exit,
            "CFG should have a BlockExit node for Kotlin .use {{}} block"
        );

        // Verify a Statement node has KotlinUse context
        let has_kotlin_use_ctx = cfg_graph
            .nodes
            .values()
            .any(|n| n.call_context == CallContext::KotlinUse);
        assert!(
            has_kotlin_use_ctx,
            "CFG should have a node with KotlinUse call context"
        );

        // Diagnose: for each Alloc, check which CFG node it's on and whether that node has KotlinUse context
        eprintln!("=== DIAGNOSTIC: All CFG nodes ===");
        for (id, node) in &cfg_graph.nodes {
            eprintln!(
                "CFG node id={:?} kind={:?} call_context={:?} stmt_range=({},{})",
                id,
                node.kind,
                node.call_context,
                node.stmt_range.start_byte,
                node.stmt_range.end_byte
            );
        }
        eprintln!("=== DIAGNOSTIC: All DataNodes ===");
        for dn in &data_nodes {
            eprintln!(
                "DataNode id={:?} kind={:?} name={:?} access_path={:?} range=({},{})",
                dn.id, dn.kind, dn.name, dn.access_path, dn.range.start_byte, dn.range.end_byte
            );
        }
        eprintln!("=== DIAGNOSTIC: All Alloc effects ===");
        for eff in all_effects.iter() {
            if let SemanticEffectKind::Alloc { callee, target } = &eff.kind {
                let cfg_node = cfg_graph.nodes.get(&eff.cfg_node_id);
                eprintln!(
                    "Alloc callee={} target={:?} cfg_node_id={:?} cfg_kind={:?} call_context={:?}",
                    callee,
                    target,
                    eff.cfg_node_id,
                    cfg_node.map(|n| n.kind),
                    cfg_node.map(|n| n.call_context)
                );
            }
        }
        eprintln!("=== END DIAGNOSTIC ===");

        // Assert: at least one Alloc effect exists for the resource producer.
        // Kotlin .use {} is now modeled as a context-managed block (like Python with,
        // Java try-with-resources), so ScopeExitAnalyzer DOES produce a BlockExit Free.
        let has_alloc = all_effects
            .iter()
            .any(|eff| matches!(&eff.kind, SemanticEffectKind::Alloc { .. }));
        assert!(
            has_alloc,
            "Expected at least one Alloc effect for Kotlin .use resource. \
             Found {} total effects: {:?}",
            all_effects.len(),
            all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        // Verify scope-exit Free at BlockExit for .use-managed resource
        let has_auto_free = all_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                if callee.contains("<block-exit>"))
        });
        assert!(
            has_auto_free,
            "Expected scope-exit Free at BlockExit for Kotlin .use-managed resource. \
             Found effects: {:?}",
            all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_kotlin_resource_patterns() {
        let config = ResourceOpConfig::default_for(Language::Kotlin);
        // Producers
        assert!(config.is_producer("File"));
        assert!(config.is_producer("bufferedReader"));
        assert!(config.is_producer("bufferedWriter"));
        assert!(config.is_producer("openConnection"));
        // Consumers — .use is an Exact consumer
        assert_eq!(config.is_consumer(".use"), Some(0));
        assert_eq!(config.is_consumer("file.close"), Some(0));
        assert_eq!(config.is_consumer("conn.dispose"), Some(0));
        // Non-patterns
        assert!(!config.is_producer("free"));
        assert_eq!(config.is_consumer("open"), None);
    }

    #[test]
    fn test_kotlin_map_not_use() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "map_not_use.kt",
            r#"fun process(): List<String> {
    val items = listOf("a", "b")
    return items.map { it.uppercase() }
}
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("map_not_use.kt");

        // Find the process function
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let func_sym = syms
            .iter()
            .find(|s| s.name == "process" && s.kind.as_str() == "function")
            .expect("process function not found");

        // Verify CFG nodes were extracted
        let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
        assert!(!cfg_nodes.is_empty(), "CFG should have nodes");

        let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();

        use atlas_engine::analysis::cfg_graph::CfgGraph;
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

        // Key negative assertion: NO node should have KotlinUse call context
        use atlas_engine::enums::CallContext;
        let has_kotlin_use_ctx = cfg_graph
            .nodes
            .values()
            .any(|n| n.call_context == CallContext::KotlinUse);
        assert!(
            !has_kotlin_use_ctx,
            "CFG should NOT have a node with KotlinUse call context for .map {{}} lambda"
        );

        // Key negative assertion: NO BlockExit node should be present
        use atlas_engine::enums::CfgNodeKind;
        let has_block_exit = cfg_graph
            .nodes
            .values()
            .any(|n| n.kind == CfgNodeKind::BlockExit);
        assert!(
            !has_block_exit,
            "CFG should NOT have a BlockExit node for .map {{}} lambda"
        );

        // Run compose_effects to verify NO <block-exit> Free and NO Alloc effects
        let data_nodes = store
            .find_data_nodes_by_function(&func_sym.id)
            .unwrap_or_default();
        let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
            vec![]
        } else {
            let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&all_ids)
                .unwrap_or_default()
        };

        use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
        use atlas_engine::effects::SemanticEffectKind;
        let contract = ResourceOpConfig::default_for(atlas_engine::Language::Kotlin);
        let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

        let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

        // Verify NO Alloc effects — .map is not a resource producer
        let has_alloc = all_effects
            .iter()
            .any(|eff| matches!(&eff.kind, SemanticEffectKind::Alloc { .. }));
        assert!(
            !has_alloc,
            "Expected NO Alloc effects for Kotlin .map. Found {:?}",
            all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        // Verify NO <block-exit> Free effect
        let has_block_exit_free = all_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                if callee.contains("<block-exit>"))
        });
        assert!(
            !has_block_exit_free,
            "Expected NO <block-exit> Free for Kotlin .map lambda"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Ruby block resource tests
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "ruby")]
mod ruby_tests {
    use super::*;
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::compose_effects;
    use atlas_engine::analysis::resource_ops::ResourceOpConfig;
    use atlas_engine::effects::SemanticEffectKind;
    use atlas_engine::enums::CallContext;
    use atlas_engine::enums::CfgNodeKind;

    #[test]
    fn test_ruby_block_resource() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "block_resource.rb",
            r#"def read_file
  File.open("data.txt") do |f|
    content = f.read
    puts content
  end
end
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("block_resource.rb");

        // Find the read_file method
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let func_sym = syms
            .iter()
            .find(|s| s.name == "read_file" && s.kind.as_str() == "method")
            .expect("read_file method not found");

        // Verify symbols were extracted
        assert!(!func_sym.id.to_string().is_empty());

        // Load CFG
        let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
        assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
        let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

        // Verify BlockExit node exists (emitted for Ruby block calls)
        let has_block_exit = cfg_graph
            .nodes
            .values()
            .any(|n| n.kind == CfgNodeKind::BlockExit);
        assert!(
            has_block_exit,
            "CFG should have a BlockExit node for Ruby block call"
        );

        // Verify a Statement node has RubyBlock context
        let has_ruby_block_ctx = cfg_graph
            .nodes
            .values()
            .any(|n| n.call_context == CallContext::RubyBlock);
        assert!(
            has_ruby_block_ctx,
            "CFG should have a node with RubyBlock call context"
        );

        // Run compose_effects to verify resource operation detection
        let data_nodes = store
            .find_data_nodes_by_function(&func_sym.id)
            .unwrap_or_default();
        let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
            vec![]
        } else {
            let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&all_ids)
                .unwrap_or_default()
        };

        let contract = ResourceOpConfig::default_for(Language::Ruby);
        let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

        let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

        // Verify File.open produces an Alloc effect
        let has_alloc = all_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "File.open")
        });
        assert!(
            has_alloc,
            "Expected Alloc effect for File.open. Found effects: {:?}",
            all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        // Verify scope-exit Free at BlockExit (auto-free for block-managed resource)
        let has_auto_free = all_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<block-exit>"))
        });
        assert!(
            has_auto_free,
            "Expected scope-exit Free at BlockExit for Ruby block-managed File.open"
        );
    }

    #[test]
    fn test_ruby_resource_patterns() {
        let config = ResourceOpConfig::default_for(Language::Ruby);
        // Producers — Exact matches take priority
        assert!(config.is_producer("File.open"));
        assert!(config.is_producer("File.new"));
        assert!(config.is_producer("TCPSocket.new"));
        assert!(config.is_producer("Net::HTTP.start"));
        assert!(config.is_producer("IO.open"));
        assert!(config.is_producer("Tempfile.create"));
        assert!(config.is_producer("Dir.chdir"));
        // Suffix matches
        assert!(config.is_producer("some.open"));
        assert!(config.is_producer("obj.new"));
        // Consumers
        assert_eq!(config.is_consumer(".close"), Some(0));
        assert_eq!(config.is_consumer("file.close"), Some(0));
        assert_eq!(config.is_consumer(".dispose"), Some(0));
        assert_eq!(config.is_consumer("obj.dispose"), Some(0));
        // Non-patterns
        assert!(!config.is_producer("free"));
    }

    #[test]
    fn test_ruby_times_not_resource() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "times_not_resource.rb",
            r#"def greet
  3.times { puts "hello" }
end
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("times_not_resource.rb");

        // Find the greet method
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let func_sym = syms
            .iter()
            .find(|s| s.name == "greet" && s.kind.as_str() == "method")
            .expect("greet method not found");

        // Verify symbols were extracted
        assert!(!func_sym.id.to_string().is_empty());

        // Load CFG
        let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
        assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
        let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

        // Key negative assertion: NO node should have RubyBlock call context
        let has_ruby_block_ctx = cfg_graph
            .nodes
            .values()
            .any(|n| n.call_context == CallContext::RubyBlock);
        assert!(
            !has_ruby_block_ctx,
            "CFG should NOT have a node with RubyBlock call context for .times {{}} block"
        );

        // Key negative assertion: NO BlockExit node should be present
        let has_block_exit = cfg_graph
            .nodes
            .values()
            .any(|n| n.kind == CfgNodeKind::BlockExit);
        assert!(
            !has_block_exit,
            "CFG should NOT have a BlockExit node for .times {{}} block"
        );

        // Run compose_effects to verify NO <block-exit> Free effect
        let data_nodes = store
            .find_data_nodes_by_function(&func_sym.id)
            .unwrap_or_default();
        let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
            vec![]
        } else {
            let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&all_ids)
                .unwrap_or_default()
        };

        let contract = ResourceOpConfig::default_for(Language::Ruby);
        let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

        let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

        // Verify NO <block-exit> Free effect
        let has_block_exit_free = all_effects.iter().any(|eff| {
            matches!(&eff.kind, SemanticEffectKind::Free { callee, .. }
                if callee.contains("<block-exit>"))
        });
        assert!(
            !has_block_exit_free,
            "Expected NO <block-exit> Free for Ruby .times block"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Scope-Exit Cleanup Language Policies (P1#5)
// ────────────────────────────────────────────────────────────────
// These tests verify the per-language/per-pattern cleanup eligibility
// mechanism: compose_effects → run_scope_exit_pass only generates
// implicit Free effects when the language AND pattern both allow it,
// or when a context-managed block (PythonWith, JavaTryWith, CSharpUsing)
// forces cleanup regardless of eligibility.

/// Python `open()` without `with` block should produce an Alloc effect
/// but NO implicit Free at exit (Python is a GC language without
/// deterministic scope cleanup; only PythonWith context forces cleanup).
#[test]
fn test_python_open_without_with_no_auto_free() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "no_with.py",
        "def read_file():\n    f = open(\"test.txt\")\n    return f.read()\n",
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("no_with.py");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let func_sym = syms
        .iter()
        .find(|s| s.name == "read_file")
        .expect("read_file function not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
    let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&func_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with Python contract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::Python);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert that open() produces an Alloc effect
    let has_alloc_for_open = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "open"),
    );
    assert!(
        has_alloc_for_open,
        "Expected an Alloc effect for open() in Python without 'with'. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert NO Free effect is generated — Python open() without 'with'
    // should NOT get implicit scope-exit cleanup (implicit_scope_cleanup=false).
    let has_free = all_effects
        .iter()
        .any(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
    assert!(
        !has_free,
        "Expected NO implicit Free effect for open() without with-block. \
         Python should not auto-free plain open() calls. \
         Found Free effects: {:?}",
        all_effects
            .iter()
            .filter(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }))
            .map(|e| &e.kind)
            .collect::<Vec<_>>()
    );

    // Verify the Alloc effect is marked as NOT eligible for implicit cleanup
    let open_alloc = all_effects.iter().find(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "open"),
    );
    assert!(
        open_alloc.is_some_and(|e| e.eligible_for_implicit_cleanup == Some(false)),
        "Alloc for open() should have eligible_for_implicit_cleanup == Some(false), \
         got {:?}",
        open_alloc.map(|e| e.eligible_for_implicit_cleanup)
    );
}

/// C `malloc()` without `free()` should produce an Alloc effect but NO
/// implicit Free at exit — C is a language without deterministic scope
/// cleanup (implicit_scope_cleanup=false).  Memory management is manual.
#[cfg(feature = "c")]
#[test]
fn test_c_malloc_without_free_no_auto_free() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "alloc_no_free.c",
        "void f() {\n    void* p = malloc(16);\n}\n",
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("alloc_no_free.c");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let func_sym = syms
        .iter()
        .find(|s| s.name == "f")
        .expect("function f not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
    let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&func_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with C contract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::C);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert that malloc() produces an Alloc effect
    let has_alloc_for_malloc = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        has_alloc_for_malloc,
        "Expected an Alloc effect for malloc() in C. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert NO Free effect — C should NOT auto-free unfreed malloc()
    // (manual deallocation required, implicit_scope_cleanup=false).
    let has_free = all_effects
        .iter()
        .any(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
    assert!(
        !has_free,
        "Expected NO implicit Free effect for malloc() without free() in C. \
         C requires manual free(); no scope-exit cleanup. \
         Found Free effects: {:?}",
        all_effects
            .iter()
            .filter(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }))
            .map(|e| &e.kind)
            .collect::<Vec<_>>()
    );

    // Verify the Alloc is explicitly ineligible
    let malloc_alloc = all_effects.iter().find(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        malloc_alloc.is_some_and(|e| e.eligible_for_implicit_cleanup == Some(false)),
        "Alloc for malloc() should have eligible_for_implicit_cleanup == Some(false), \
         got {:?}",
        malloc_alloc.map(|e| e.eligible_for_implicit_cleanup)
    );
}

/// C++ `malloc()` without `free()` should NOT get implicit scope-exit Free.
/// C++ has implicit_scope_cleanup=true at the language level (RAII), but
/// individual C API patterns like `malloc` are explicitly marked
/// `implicit_cleanup: false` — manual deallocation is still required for
/// C library calls, even in C++ code.
#[cfg(feature = "cpp")]
#[test]
fn test_cpp_malloc_without_free_no_auto_free() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "alloc_no_free.cpp",
        "void f() {\n    void* p = malloc(16);\n}\n",
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("alloc_no_free.cpp");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let func_sym = syms
        .iter()
        .find(|s| s.name == "f")
        .expect("function f not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
    let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&func_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with C++ contract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::Cpp);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert that malloc() produces an Alloc effect
    let has_alloc_for_malloc = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        has_alloc_for_malloc,
        "Expected an Alloc effect for malloc() in C++. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert NO implicit Free — even though C++ has RAII at the language
    // level, malloc() is a C API pattern explicitly marked ineligible.
    let has_free = all_effects
        .iter()
        .any(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
    assert!(
        !has_free,
        "Expected NO implicit Free effect for malloc() without free() in C++. \
         malloc() has implicit_cleanup=false in C++ patterns, even though \
         C++ has implicit_scope_cleanup=true at the language level. \
         Found Free effects: {:?}",
        all_effects
            .iter()
            .filter(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }))
            .map(|e| &e.kind)
            .collect::<Vec<_>>()
    );

    // The Alloc must carry eligible_for_implicit_cleanup == Some(false)
    // — the per-pattern flag overrides the language-level default.
    let malloc_alloc = all_effects.iter().find(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        malloc_alloc.is_some_and(|e| e.eligible_for_implicit_cleanup == Some(false)),
        "Alloc for malloc() in C++ should have eligible_for_implicit_cleanup == Some(false). \
         The per-pattern implicit_cleanup:false must override the C++ language-level \
         implicit_scope_cleanup:true. Got {:?}",
        malloc_alloc.map(|e| e.eligible_for_implicit_cleanup)
    );
}

// ────────────────────────────────────────────────────────────────
// PHP procedural resource tests
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "php")]
mod php_tests {
    use super::*;
    use atlas_engine::analysis::resource_ops::ResourceOpConfig;

    #[test]
    fn test_php_procedural_resource() {
        let _ = tracing_subscriber::fmt::try_init();

        let files = &[(
            "procedural_resource.php",
            r#"<?php
function read_file() {
    $handle = fopen("data.txt", "r");
    if ($handle) {
        $content = fread($handle, filesize("data.txt"));
        fclose($handle);
    }
}
"#,
        )];

        let (store, _stats) = index_files(files);
        let file_id = FileId::generate("procedural_resource.php");

        // Find the read_file function
        let syms = store.find_symbols_by_file(&file_id).unwrap();
        let func_sym = syms
            .iter()
            .find(|s| s.name == "read_file" && s.kind.as_str() == "function")
            .expect("read_file function not found");

        // Verify symbols were extracted
        assert!(!func_sym.id.to_string().is_empty());
    }

    #[test]
    fn test_php_resource_patterns() {
        let config = ResourceOpConfig::default_for(Language::Php);
        // Producers — Exact matches
        assert!(config.is_producer("fopen"));
        assert!(config.is_producer("mysqli_connect"));
        assert!(config.is_producer("curl_init"));
        // Suffix connect
        assert!(config.is_producer("db_connect"));
        // Consumers — Exact matches
        assert_eq!(config.is_consumer("fclose"), Some(0));
        assert_eq!(config.is_consumer("mysqli_close"), Some(0));
        assert_eq!(config.is_consumer("curl_close"), Some(0));
        // Suffix close
        assert_eq!(config.is_consumer("handle_close"), Some(0));
        // Non-patterns
        assert!(!config.is_producer("free"));
        assert_eq!(config.is_consumer("open"), None);
    }
}

// ────────────────────────────────────────────────────────────────
// C E2E Semantic Effects Tests
// ────────────────────────────────────────────────────────────────

/// C `malloc()` without `free()` — full pipeline from parse through
/// compose_effects.  Iterates over all function symbols in the file.
/// C has no deterministic scope cleanup, so no implicit Free is expected.
#[cfg(feature = "c")]
#[test]
fn test_c_parse_to_effects_no_auto_free() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[("test.c", "void f() {\n    void* p = malloc(16);\n}\n")];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("test.c");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let fn_syms: Vec<_> = syms
        .iter()
        .filter(|s| matches!(s.kind, atlas_engine::SymbolKind::Function))
        .collect();
    assert!(!fn_syms.is_empty(), "Expected at least one function symbol");

    let contract = ResourceOpConfig::default_for(atlas_engine::Language::C);
    let mut all_effects: Vec<atlas_engine::effects::SemanticEffect> = Vec::new();

    for sym in &fn_syms {
        let cfg_nodes = store
            .find_cfg_nodes_by_function(&sym.id)
            .unwrap_or_default();
        if cfg_nodes.is_empty() {
            continue;
        }
        let cfg_edges = store
            .find_cfg_edges_by_function(&sym.id)
            .unwrap_or_default();
        let cfg_graph =
            CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");
        let data_nodes = store
            .find_data_nodes_by_function(&sym.id)
            .unwrap_or_default();
        let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
            vec![]
        } else {
            let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&all_ids)
                .unwrap_or_default()
        };

        if cfg_graph.nodes.is_empty() {
            continue;
        }

        let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);
        all_effects.extend(composition.node_effects.values().flatten().cloned());
    }

    // Assert: Alloc effect EXISTS for malloc
    let has_alloc_for_malloc = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        has_alloc_for_malloc,
        "Expected an Alloc effect for malloc() in C. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert: NO Free effect is produced (no implicit cleanup in C)
    let has_free = all_effects
        .iter()
        .any(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
    assert!(
        !has_free,
        "Expected NO implicit Free effect for malloc() without free() in C. \
         C requires manual free(); no scope-exit cleanup. \
         Found Free effects: {:?}",
        all_effects
            .iter()
            .filter(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }))
            .map(|e| &e.kind)
            .collect::<Vec<_>>()
    );

    // Assert: Alloc has eligible_for_implicit_cleanup == Some(false)
    let malloc_alloc = all_effects.iter().find(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        malloc_alloc.is_some_and(|e| e.eligible_for_implicit_cleanup == Some(false)),
        "Alloc for malloc() should have eligible_for_implicit_cleanup == Some(false), \
         got {:?}",
        malloc_alloc.map(|e| e.eligible_for_implicit_cleanup)
    );
}

/// C `malloc()` with explicit `free(p)` in the same function.
/// Verifies that both Alloc and Free effects are produced, and the Free
/// carries ConsumptionStyle::ExplicitCall (not Deferred or ContextManaged).
#[cfg(feature = "c")]
#[test]
fn test_c_parse_to_effects_with_explicit_free() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::{ConsumptionStyle, SemanticEffectKind};

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "with_free.c",
        "void f() {\n    void* p = malloc(16);\n    free(p);\n}\n",
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("with_free.c");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let func_sym = syms
        .iter()
        .find(|s| s.name == "f")
        .expect("function f not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
    let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&func_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with C contract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::C);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert: Alloc exists for malloc
    let has_alloc_for_malloc = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "malloc"),
    );
    assert!(
        has_alloc_for_malloc,
        "Expected an Alloc effect for malloc(). \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert: Free exists for free(p)
    let free_effect = all_effects.iter().find(
        |eff| matches!(&eff.kind, SemanticEffectKind::Free { callee, .. } if callee == "free"),
    );
    assert!(
        free_effect.is_some(),
        "Expected a Free effect for free(p). \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert: Free has consumption_style == ExplicitCall
    assert_eq!(
        free_effect.unwrap().consumption_style,
        Some(ConsumptionStyle::ExplicitCall),
        "Free for free(p) should have ExplicitCall consumption style, got {:?}",
        free_effect.unwrap().consumption_style
    );
}

/// C++ code calling the C API `fopen()` without `fclose()` should produce
/// an Alloc but NO implicit Free.  Even though C++ has RAII at the language
/// level, C API patterns like `fopen` are explicitly marked ineligible for
/// implicit cleanup — manual `fclose()` is still required.
#[cfg(feature = "cpp")]
#[test]
fn test_cpp_c_api_no_auto_free() {
    use atlas_engine::analysis::cfg_graph::CfgGraph;
    use atlas_engine::analysis::{ResourceOpConfig, compose_effects};
    use atlas_engine::effects::SemanticEffectKind;

    let _ = tracing_subscriber::fmt::try_init();
    let files = &[(
        "fopen_test.cpp",
        "#include <cstdio>\nvoid f() {\n    FILE* fp = fopen(\"test.txt\", \"r\");\n}\n",
    )];

    let (store, _stats) = index_files(files);
    let file_id = FileId::generate("fopen_test.cpp");

    let syms = store.find_symbols_by_file(&file_id).unwrap();
    let func_sym = syms
        .iter()
        .find(|s| s.name == "f")
        .expect("function f not found");

    // Load CFG
    let cfg_nodes = store.find_cfg_nodes_by_function(&func_sym.id).unwrap();
    assert!(!cfg_nodes.is_empty(), "CFG should have nodes");
    let cfg_edges = store.find_cfg_edges_by_function(&func_sym.id).unwrap();
    let cfg_graph = CfgGraph::build(&cfg_nodes, &cfg_edges).expect("CfgGraph build should succeed");

    // Load DataFlow
    let data_nodes = store.find_data_nodes_by_function(&func_sym.id).unwrap();
    let dataflow_edges: Vec<_> = if data_nodes.is_empty() {
        vec![]
    } else {
        let all_ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
        store
            .find_dataflow_edges_by_sources(&all_ids)
            .unwrap_or_default()
    };

    // Run compose_effects with C++ contract
    let contract = ResourceOpConfig::default_for(atlas_engine::Language::Cpp);
    let composition = compose_effects(&cfg_graph, &data_nodes, &dataflow_edges, &contract);

    let all_effects: Vec<_> = composition.node_effects.values().flatten().collect();

    // Assert: Alloc exists for fopen
    let has_alloc_for_fopen = all_effects.iter().any(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "fopen"),
    );
    assert!(
        has_alloc_for_fopen,
        "Expected an Alloc effect for fopen() in C++. \
         Found {} total effects: {:?}",
        all_effects.len(),
        all_effects.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );

    // Assert: NO Free for fclose (C API pattern excluded from C++ auto-free)
    let has_free = all_effects
        .iter()
        .any(|eff| matches!(&eff.kind, SemanticEffectKind::Free { .. }));
    assert!(
        !has_free,
        "Expected NO implicit Free effect for fopen() without fclose() in C++. \
         C API patterns like fopen are explicitly excluded from C++ implicit cleanup. \
         Found Free effects: {:?}",
        all_effects
            .iter()
            .filter(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }))
            .map(|e| &e.kind)
            .collect::<Vec<_>>()
    );

    // Assert: Alloc has eligible_for_implicit_cleanup == Some(false)
    let fopen_alloc = all_effects.iter().find(
        |eff| matches!(&eff.kind, SemanticEffectKind::Alloc { callee, .. } if callee == "fopen"),
    );
    assert!(
        fopen_alloc.is_some_and(|e| e.eligible_for_implicit_cleanup == Some(false)),
        "Alloc for fopen() should have eligible_for_implicit_cleanup == Some(false), \
         got {:?}",
        fopen_alloc.map(|e| e.eligible_for_implicit_cleanup)
    );
}
