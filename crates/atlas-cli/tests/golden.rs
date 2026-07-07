//! Golden test framework for Atlas extraction pipeline.
//!
//! Each language fixture is a source file + expected JSON output.
//! The test extracts the source file, serializes key fields, and compares
//! against the expected JSON.
//!
//! ## Adding a new fixture
//!
//! 1. Create `tests/fixtures/<lang>/<name>.<ext>` with source code
//! 2. Run `cargo test --test golden -- --nocapture`
//! 3. Copy the printed JSON to `tests/fixtures/<lang>/<name>.expected.json`
//!
//! ## Expected JSON format
//!
//! ```json
//! {
//!   "symbols": [{ "name": "foo", "kind": "function", "signature": "(arg: Type): Return" }],
//!   "references": [{ "text": "bar", "kind": "call" }],
//!   "imports": [{ "module": "express", "imported_name": "Router" }],
//!   "scopes": [{ "name": "Server", "kind": "class" }],
//!   "callsites": [{ "caller": "Server.start", "receiver": "router" }]
//! }
//! ```

use atlas_engine::enums::Language;
use atlas_engine::ids::FileId;
use atlas_engine::{ExtractionMode, LanguageFrontend, extract_file_with_mode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Expected output types (simplified, for readable diff)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GoldenExpected {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    symbols: Vec<GldSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    references: Vec<GldReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    imports: Vec<GldImport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<GldScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    callsites: Vec<GldCallsite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cfg_nodes: Vec<GldCfgNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cfg_edges: Vec<GldCfgEdge>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldSymbol {
    name: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldReference {
    text: String,
    kind: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldImport {
    module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    imported_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldScope {
    name: String,
    kind: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldCallsite {
    caller: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receiver: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldCfgNode {
    kind: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GldCfgEdge {
    source_kind: String,
    target_kind: String,
    kind: String,
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
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

/// Run a golden test for the given fixture.
///
/// - `lang_dir`: e.g. "typescript"
/// - `stem`: e.g. "simple" (matches `simple.ts` and `simple.expected.json`)
fn run_golden(lang_dir: &str, stem: &str, ext: &str, lang: Language) {
    let dir = fixtures_dir().join(lang_dir);
    let src_path = dir.join(format!("{stem}.{ext}"));
    let expected_path = dir.join(format!("{stem}.expected.json"));

    let source = std::fs::read_to_string(&src_path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {}", src_path.display(), e));

    let rel_path = format!("{lang_dir}/{stem}.{ext}");
    let frontend =
        atlas_engine::create_frontend(lang).unwrap_or_else(|| panic!("No frontend for {lang:?}"));

    let file_id = FileId::generate(&rel_path);
    let facts = extract_full(
        &frontend,
        file_id,
        Path::new(&rel_path),
        &source,
        "test_hash",
    )
    .expect("Extraction failed");

    // Convert to simplified golden format
    let actual = GoldenExpected {
        symbols: facts
            .symbols
            .iter()
            .map(|s| GldSymbol {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
                signature: s.signature.clone(),
            })
            .collect(),
        references: facts
            .references
            .iter()
            .map(|r| GldReference {
                text: r.text.clone(),
                kind: r.kind.as_str().to_string(),
            })
            .collect(),
        imports: facts
            .imports
            .iter()
            .map(|i| GldImport {
                module: i.module.clone(),
                imported_name: if i.imported_name.is_empty() {
                    None
                } else {
                    Some(i.imported_name.clone())
                },
            })
            .collect(),
        scopes: facts
            .scopes
            .iter()
            .map(|s| GldScope {
                name: s.name.clone(),
                kind: s.kind.as_str().to_string(),
            })
            .collect(),
        callsites: facts
            .callsites
            .iter()
            .map(|c| {
                // Find caller name from symbols
                let caller_name = facts
                    .symbols
                    .iter()
                    .find(|s| s.id == c.caller)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| format!("{:?}", c.caller));
                GldCallsite {
                    caller: caller_name,
                    receiver: c.receiver.clone(),
                }
            })
            .collect(),
        cfg_nodes: facts
            .cfg_nodes
            .iter()
            .map(|n| GldCfgNode {
                kind: n.kind.as_str().to_string(),
            })
            .collect(),
        cfg_edges: facts
            .cfg_edges
            .iter()
            .map(|e| {
                // Resolve source/target node kinds from the CFG nodes list
                let source_kind = facts
                    .cfg_nodes
                    .iter()
                    .find(|n| n.id == e.source)
                    .map(|n| n.kind.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let target_kind = facts
                    .cfg_nodes
                    .iter()
                    .find(|n| n.id == e.target)
                    .map(|n| n.kind.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                GldCfgEdge {
                    source_kind,
                    target_kind,
                    kind: e.kind.as_str().to_string(),
                }
            })
            .collect(),
    };

    let actual_json = serde_json::to_string_pretty(&actual).unwrap();
    if std::env::var_os("ATLAS_UPDATE_GOLDEN").is_some() || !expected_path.exists() {
        // Bootstrap/update mode: write actual output as the expected baseline.
        let json = serde_json::to_string_pretty(&actual).unwrap();
        std::fs::write(&expected_path, &json)
            .unwrap_or_else(|e| panic!("Cannot write {}: {}", expected_path.display(), e));
        return;
    }

    let expected_json = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {}", expected_path.display(), e));
    let expected: GoldenExpected = serde_json::from_str(&expected_json)
        .unwrap_or_else(|e| panic!("Cannot parse {}: {}", expected_path.display(), e));

    if actual != expected {
        panic!(
            "\nGolden test mismatch: {lang_dir}/{stem}\n\n--- Expected ---\n{expected_json}\n\n--- Actual ---\n{actual_json}\n"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "typescript")]
#[test]
fn golden_typescript_simple() {
    run_golden("typescript", "simple", "ts", Language::TypeScript);
}

#[cfg(feature = "python")]
#[test]
fn golden_python_simple() {
    run_golden("python", "simple", "py", Language::Python);
}

#[cfg(feature = "javascript")]
#[test]
fn golden_javascript_simple() {
    run_golden("javascript", "simple", "js", Language::JavaScript);
}

#[cfg(feature = "arkts")]
#[test]
fn golden_arkts_simple() {
    run_golden("arkts", "simple", "ets", Language::ArkTS);
}

#[cfg(feature = "arkts")]
#[test]
fn golden_arkts_struct_simple() {
    run_golden("arkts", "struct_simple", "ets", Language::ArkTS);
}

#[cfg(feature = "arkts")]
#[test]
fn golden_arkts_struct_complex() {
    run_golden("arkts", "struct_complex", "ets", Language::ArkTS);
}

// ---------------------------------------------------------------------------
// P2: Import resolution golden tests
// ---------------------------------------------------------------------------

#[cfg(feature = "typescript")]
#[test]
fn golden_typescript_imports() {
    run_golden("typescript", "imports", "ts", Language::TypeScript);
}

#[cfg(feature = "c")]
#[test]
fn golden_c_includes() {
    run_golden("c", "includes", "c", Language::C);
}

// ---------------------------------------------------------------------------
// P4: CFG golden tests
// ---------------------------------------------------------------------------

#[cfg(feature = "typescript")]
#[test]
fn golden_typescript_cfg() {
    run_golden("typescript", "cfg", "ts", Language::TypeScript);
}

#[cfg(feature = "typescript")]
#[test]
fn golden_typescript_cfg_if_else() {
    run_golden("typescript", "cfg_if_else", "ts", Language::TypeScript);
}

#[cfg(feature = "typescript")]
#[test]
fn golden_typescript_cfg_loop() {
    run_golden("typescript", "cfg_loop", "ts", Language::TypeScript);
}

#[cfg(feature = "python")]
#[test]
fn golden_python_cfg_if_else() {
    run_golden("python", "cfg_if_else", "py", Language::Python);
}

#[cfg(feature = "python")]
#[test]
fn golden_python_cfg_loop() {
    run_golden("python", "cfg_loop", "py", Language::Python);
}

#[cfg(feature = "go")]
#[test]
fn golden_go_cfg_if_else() {
    run_golden("go", "cfg_if_else", "go", Language::Go);
}

#[cfg(feature = "go")]
#[test]
fn golden_go_cfg_loop() {
    run_golden("go", "cfg_loop", "go", Language::Go);
}

#[cfg(feature = "rust")]
#[test]
fn golden_rust_cfg_if_else() {
    run_golden("rust", "cfg_if_else", "rs", Language::Rust);
}

#[cfg(feature = "rust")]
#[test]
fn golden_rust_cfg_loop() {
    run_golden("rust", "cfg_loop", "rs", Language::Rust);
}

// ---------------------------------------------------------------------------
// Post-MVP language golden tests (Symbolic level)
// ---------------------------------------------------------------------------

// -- Go --

#[cfg(feature = "go")]
#[test]
fn golden_go_simple() {
    run_golden("go", "simple", "go", Language::Go);
}

#[cfg(feature = "go")]
#[test]
fn golden_go_imports() {
    run_golden("go", "imports", "go", Language::Go);
}

#[cfg(feature = "go")]
#[test]
fn golden_go_calls() {
    run_golden("go", "calls", "go", Language::Go);
}

#[cfg(feature = "go")]
#[test]
fn golden_go_class() {
    run_golden("go", "class", "go", Language::Go);
}

// -- C# --

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_simple() {
    run_golden("csharp", "simple", "cs", Language::CSharp);
}

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_imports() {
    run_golden("csharp", "imports", "cs", Language::CSharp);
}

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_calls() {
    run_golden("csharp", "calls", "cs", Language::CSharp);
}

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_class() {
    run_golden("csharp", "class", "cs", Language::CSharp);
}

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_cfg_if_else() {
    run_golden("csharp", "cfg_if_else", "cs", Language::CSharp);
}

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_cfg_loop() {
    run_golden("csharp", "cfg_loop", "cs", Language::CSharp);
}

// -- Rust --

#[cfg(feature = "rust")]
#[test]
fn golden_rust_simple() {
    run_golden("rust", "simple", "rs", Language::Rust);
}

#[cfg(feature = "rust")]
#[test]
fn golden_rust_imports() {
    run_golden("rust", "imports", "rs", Language::Rust);
}

#[cfg(feature = "rust")]
#[test]
fn golden_rust_calls() {
    run_golden("rust", "calls", "rs", Language::Rust);
}

#[cfg(feature = "rust")]
#[test]
fn golden_rust_class() {
    run_golden("rust", "class", "rs", Language::Rust);
}

// -- PHP --

#[cfg(feature = "php")]
#[test]
fn golden_php_simple() {
    run_golden("php", "simple", "php", Language::Php);
}

#[cfg(feature = "php")]
#[test]
fn golden_php_imports() {
    run_golden("php", "imports", "php", Language::Php);
}

#[cfg(feature = "php")]
#[test]
fn golden_php_calls() {
    run_golden("php", "calls", "php", Language::Php);
}

#[cfg(feature = "php")]
#[test]
fn golden_php_class() {
    run_golden("php", "class", "php", Language::Php);
}

// -- Ruby --

#[cfg(feature = "ruby")]
#[test]
fn golden_ruby_simple() {
    run_golden("ruby", "simple", "rb", Language::Ruby);
}

#[cfg(feature = "ruby")]
#[test]
fn golden_ruby_imports() {
    run_golden("ruby", "imports", "rb", Language::Ruby);
}

#[cfg(feature = "ruby")]
#[test]
fn golden_ruby_calls() {
    run_golden("ruby", "calls", "rb", Language::Ruby);
}

#[cfg(feature = "ruby")]
#[test]
fn golden_ruby_class() {
    run_golden("ruby", "class", "rb", Language::Ruby);
}

// -- Kotlin --

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_simple() {
    run_golden("kotlin", "simple", "kt", Language::Kotlin);
}

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_imports() {
    run_golden("kotlin", "imports", "kt", Language::Kotlin);
}

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_calls() {
    run_golden("kotlin", "calls", "kt", Language::Kotlin);
}

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_class() {
    run_golden("kotlin", "class", "kt", Language::Kotlin);
}

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_cfg_if_else() {
    run_golden("kotlin", "cfg_if_else", "kt", Language::Kotlin);
}

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_cfg_loop() {
    run_golden("kotlin", "cfg_loop", "kt", Language::Kotlin);
}

// -- Java (Symbolic + Dataflow, opt-in) --

#[cfg(feature = "java")]
#[test]
fn golden_java_cfg_if_else() {
    run_golden("java", "cfg_if_else", "java", Language::Java);
}

#[cfg(feature = "java")]
#[test]
fn golden_java_cfg_loop() {
    run_golden("java", "cfg_loop", "java", Language::Java);
}

// -- C (Symbolic + Dataflow, opt-in) --

#[cfg(feature = "c")]
#[test]
fn golden_c_cfg_if_else() {
    run_golden("c", "cfg_if_else", "c", Language::C);
}

#[cfg(feature = "c")]
#[test]
fn golden_c_cfg_loop() {
    run_golden("c", "cfg_loop", "c", Language::C);
}

// -- C++ (Symbolic + Dataflow, opt-in) --

#[cfg(feature = "cpp")]
#[test]
fn golden_cpp_cfg_if_else() {
    run_golden("cpp", "cfg_if_else", "cpp", Language::Cpp);
}

#[cfg(feature = "cpp")]
#[test]
fn golden_cpp_cfg_loop() {
    run_golden("cpp", "cfg_loop", "cpp", Language::Cpp);
}

// -- Cangjie (Symbolic, opt-in, reduced fixtures) --

#[cfg(feature = "cangjie")]
#[test]
fn golden_cangjie_simple() {
    run_golden("cangjie", "simple", "cj", Language::Cangjie);
}

#[cfg(feature = "cangjie")]
#[test]
fn golden_cangjie_imports() {
    run_golden("cangjie", "imports", "cj", Language::Cangjie);
}

#[cfg(feature = "cangjie")]
#[test]
fn golden_cangjie_calls() {
    run_golden("cangjie", "calls", "cj", Language::Cangjie);
}

// ---------------------------------------------------------------------------
// Lifecycle/Semantic fixture golden tests (extraction-level)
// ---------------------------------------------------------------------------

#[cfg(feature = "go")]
#[test]
fn golden_go_goroutine() {
    run_golden("go", "goroutine", "go", Language::Go);
}

#[cfg(feature = "rust")]
#[test]
fn golden_rust_scope_exit() {
    run_golden("rust", "scope_exit", "rs", Language::Rust);
}

#[cfg(feature = "java")]
#[test]
fn golden_java_try_resource() {
    run_golden("java", "try_resource", "java", Language::Java);
}

#[cfg(feature = "csharp")]
#[test]
fn golden_csharp_using_dispose() {
    run_golden("csharp", "using_dispose", "cs", Language::CSharp);
}

#[cfg(feature = "kotlin")]
#[test]
fn golden_kotlin_use_resource() {
    run_golden("kotlin", "use_resource", "kt", Language::Kotlin);
}

#[cfg(feature = "ruby")]
#[test]
fn golden_ruby_block_resource() {
    run_golden("ruby", "block_resource", "rb", Language::Ruby);
}

#[cfg(feature = "php")]
#[test]
fn golden_php_procedural_resource() {
    run_golden("php", "procedural_resource", "php", Language::Php);
}
