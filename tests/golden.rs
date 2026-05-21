//! Golden test framework for Atlas extraction pipeline.
//!
//! Each language fixture is a source file + expected JSON output.
//! The test extracts the source file, serializes key fields, and compares
//! against the expected JSON.
//!
//! ## Adding a new fixture
//!
//! 1. Create `tests/fixtures/<lang>/<name>.<ext>` with source code
//! 2. Run `cargo test --test golden --features all-languages -- --nocapture`
//! 3. Copy the printed JSON to `tests/fixtures/<lang>/<name>.expected.json`
//!
//! ## Expected JSON format
//!
//! ```json
//! {
//!   "symbols": [{ "name": "foo", "kind": "function" }],
//!   "references": [{ "text": "bar", "kind": "call" }],
//!   "imports": [{ "module": "express", "imported_name": "Router" }],
//!   "scopes": [{ "name": "Server", "kind": "class" }],
//!   "callsites": [{ "caller": "Server.start", "receiver": "router" }]
//! }
//! ```

use atlas::extraction::extract_file;
use atlas::types::enums::Language;
use atlas::types::ids::FileId;
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

/// Run a golden test for the given fixture.
///
/// - `lang_dir`: e.g. "typescript"
/// - `stem`: e.g. "simple" (matches `simple.ts` and `simple.expected.json`)
fn run_golden(lang_dir: &str, stem: &str, ext: &str, lang: Language) {
    let dir = fixtures_dir().join(lang_dir);
    let src_path = dir.join(format!("{}.{}", stem, ext));
    let expected_path = dir.join(format!("{}.expected.json", stem));

    let source = std::fs::read_to_string(&src_path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {}", src_path.display(), e));

    let rel_path = format!("{}/{}.{}", lang_dir, stem, ext);
    let frontend = atlas::extraction::create_frontend(lang)
        .unwrap_or_else(|| panic!("No frontend for {:?}", lang));

    let file_id = FileId::generate(&rel_path);
    let facts = extract_file(
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

    if !expected_path.exists() {
        // No expected file yet — write actual output as the expected file
        // (bootstrap mode: first run creates the baseline)
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
        let actual_json = serde_json::to_string_pretty(&actual).unwrap();
        panic!(
            "\nGolden test mismatch: {}/{}\n\n--- Expected ---\n{}\n\n--- Actual ---\n{}\n",
            lang_dir, stem, expected_json, actual_json
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
