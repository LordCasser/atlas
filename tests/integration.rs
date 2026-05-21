//! Integration tests for Atlas — end-to-end multi-file pipelines.
//!
//! These tests create temporary directories, write source files, run the
//! full extraction→storage→resolution→graph pipeline, and verify results.
//!
//! Run with default features:  `cargo test --test integration`
//! Run with all languages:    `cargo test --test integration --features all-languages,mcp,sync`

use atlas::db::Store;
use atlas::extraction::extract_file;
use atlas::graph::GraphBuilder;
use atlas::resolution::{ReferenceResolver, ResolutionStats};
use atlas::types::enums::{EdgeKind, Language};
use atlas::types::ids::FileId;
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
        let frontend = atlas::extraction::create_frontend(lang)
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
    use atlas::graph::GraphEngine;

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
// MCP Server Integration Test (feature-gated)
// ────────────────────────────────────────────────────────────────

#[cfg(feature = "mcp")]
#[test]
fn mcp_tools_are_registered() {
    let _ = tracing_subscriber::fmt::try_init();

    let tools = atlas::mcp::make_all_tools();
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
