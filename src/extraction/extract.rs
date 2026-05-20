//! Extractor: orchestrates tree-sitter parsing + LanguageAdapter normalization → FileFacts.
//!
//! The extractor:
//! 1. Parses source code with tree-sitter
//! 2. Runs the 4 queries (definitions, references, imports, scopes)
//! 3. Calls `normalize_*()` on each capture — adapter converts raw nodes into Atlas IR
//! 4. Assembles FileFacts (structural edges left to resolver phase)

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

use crate::types::{
    Callsite, DiagnosticLevel, ExtractDiagnostic, FileFacts, FileInfo,
    ParseStatus, ReferenceKind, SymbolDef,
};
use crate::types::dataflow::DataNode;
use crate::types::enums::SymbolKind;
use crate::types::ids::{CallsiteId, FileId};

use super::languages::LanguageAdapter;
use super::semantic_binder::SemanticBinder;
use super::lexical_binder::LexicalBindingResult;
use super::dataflow_builder::{DataFlowBuilder, DataFlowResult};
use super::cfg_builder::{CfgBuilder, CfgResult};

/// Extract a single file's facts using the given adapter.
pub fn extract_file(
    adapter: &dyn LanguageAdapter,
    file_id: FileId,
    file_path: &Path,
    source: &str,
    content_hash: &str,
) -> Result<FileFacts> {
    let mut diagnostics = Vec::new();

    let ts_lang = adapter.tree_sitter_language();

    // 1. Parse
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .context("Failed to set tree-sitter language")?;

    let source_bytes = source.as_bytes();
    let tree = parser
        .parse(source_bytes, None)
        .context("Failed to parse source")?;

    let root = tree.root_node();

    if root.has_error() {
        diagnostics.push(ExtractDiagnostic {
            level: DiagnosticLevel::Warning,
            message: "Parse errors detected (extraction best-effort)".into(),
            range: None,
        });
    }

    let language = adapter.language();

    // 2. Extract and normalize definitions
    let mut symbols = extract_and_normalize(
        adapter, &ts_lang, adapter.definition_query(), root, source, source_bytes,
        file_id, file_path, &mut diagnostics,
        |adapter, name, node, src, fid, fp| {
            adapter.normalize_definition(&name, node, src, fid, fp)
        },
    )?;

    // 3. Extract and normalize references
    let mut references = extract_and_normalize(
        adapter, &ts_lang, adapter.reference_query(), root, source, source_bytes,
        file_id, file_path, &mut diagnostics,
        |adapter, name, node, src, fid, fp| {
            adapter.normalize_reference(&name, node, src, fid, fp)
        },
    )?;

    // 4. Extract and normalize imports
    let imports = extract_and_normalize(
        adapter, &ts_lang, adapter.import_query(), root, source, source_bytes,
        file_id, file_path, &mut diagnostics,
        |adapter, name, node, src, fid, fp| {
            adapter.normalize_import(&name, node, src, fid, fp)
        },
    )?;

    // 5. Extract and normalize scopes
    let mut scopes = extract_and_normalize(
        adapter, &ts_lang, adapter.scope_query(), root, source, source_bytes,
        file_id, file_path, &mut diagnostics,
        |adapter, name, node, src, fid, fp| {
            adapter.normalize_scope(&name, node, src, fid, fp)
        },
    )?;

    // 6. Raw edges are now populated downstream by GraphBuilder (new P3 path).
    //    Old normalize_dataflow path was removed in favor of DataFlowBuilder.
    let mut raw_edges = Vec::new();

    // 7. Build scope tree and assign containers
    super::build_scope_tree(&mut scopes, &mut symbols);

    // 7a. Extract lexical bindings (parameters, locals, import aliases, etc.)
    //     This runs the adapter's lexical_query() to find binding definitions and uses.
    let lexical_result = super::lexical_binder::LexicalBinder::extract(
        adapter, &ts_lang, root, source, source_bytes,
        file_id, file_path, &scopes, &symbols,
    )
    .unwrap_or_else(|e| {
        diagnostics.push(ExtractDiagnostic {
            level: DiagnosticLevel::Warning,
            message: format!("Lexical binding extraction failed: {}", e),
            range: None,
        });
        LexicalBindingResult { bindings: vec![], uses: vec![] }
    });
    let bindings = lexical_result.bindings;
    let binding_uses = lexical_result.uses;

    // 7b. Build dataflow graph (DataNodes + DataFlowEdges)
    //     Runs the adapter's dataflow_builder_query() to find assignments,
    //     returns, call args, member accesses, and literals.
    let dataflow_result = super::dataflow_builder::DataFlowBuilder::extract(
        adapter, &ts_lang, root, source, source_bytes,
        file_id, file_path, &bindings, &scopes,
    )
    .unwrap_or_else(|e| {
        diagnostics.push(ExtractDiagnostic {
            level: DiagnosticLevel::Warning,
            message: format!("DataFlow builder failed: {}", e),
            range: None,
        });
        DataFlowResult::default()
    });
    let mut data_nodes = dataflow_result.nodes;
    let mut dataflow_edges = dataflow_result.edges;

    // 7c. Build per-function control-flow graphs (CfgBuilder)
    //     Matches function symbols to tree-sitter nodes, builds CFG for each.
    let cfg_result = build_cfg_for_functions(root, &symbols, source_bytes)
        .unwrap_or_else(|e| {
            diagnostics.push(ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!("CFG builder failed: {}", e),
                range: None,
            });
            CfgResult::default()
        });
    let cfg_nodes = cfg_result.nodes;
    let cfg_edges = cfg_result.edges;

    // 7d. Resolve DataNode function_ids from enclosing function symbols.
    //     DataFlowBuilder produces nodes without function_id (None).  This
    //     step walks the AST to find the enclosing function for each node
    //     and sets function_id to the matching SymbolDef.
    resolve_dataflow_function_ids(&mut data_nodes, &symbols);

    // 7e. Resolve cross-statement use-def edges.
    //     After function_ids are set, group nodes by (function_id, name)
    //     and create edges from the first definition to later uses.
    //     This enables basic taint propagation across statements.
    let use_def_edges = DataFlowBuilder::resolve_use_def(&data_nodes);
    dataflow_edges.extend(use_def_edges);

    // 8. Bind source ownership and scope through the semantic binder.
    // This is the single source of truth for references/dataflow/callsites:
    // adapters may produce best-effort source IDs, but only IDs present in
    // `symbols` are allowed to survive extraction.
    let binder = SemanticBinder::new(&symbols, &scopes);
    binder.bind_all(file_id, &mut references, &mut raw_edges);

    // 9. Derive callsites from Call references
    let callsites: Vec<Callsite> = references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Call && r.source_symbol.is_some())
        .map(|r| {
            let caller = r.source_symbol.unwrap(); // safe: filter ensures Some
            let cs_id = CallsiteId::generate(&r.id, Some(&caller), r.range.start_byte);
            Callsite {
                id: cs_id,
                reference_id: Some(r.id),
                caller,
                callee: None,      // resolved later by the resolution pipeline
                receiver: r.receiver.clone(),
                args: Vec::new(),  // arguments filled later by dataflow resolution
                range: r.range,
            }
        })
        .collect();

    // 10. Collect exported symbol IDs
    let exports: Vec<_> = symbols.iter()
        .filter(|s| s.exported)
        .map(|s| s.id)
        .collect();

    // Determine parse status
    let status = if diagnostics.iter().any(|d| d.level == DiagnosticLevel::Error) {
        ParseStatus::Error
    } else if !diagnostics.is_empty() {
        ParseStatus::Partial
    } else {
        ParseStatus::Success
    };

    Ok(FileFacts {
        file: FileInfo {
            file_id,
            path: file_path.display().to_string(),
            language,
            content_hash: content_hash.to_string(),
            status,
        },
        symbols,
        scopes,
        references,
        imports,
        exports,
        raw_edges,         // Symbol-level dataflow edges from normalize_dataflow (old path)
        callsites,         // Derived from Call references (resolved later)
        diagnostics,
        bindings,          // lexical binding definitions
        binding_uses,      // lexical binding use sites
        data_nodes,        // per-function dataflow nodes
        dataflow_edges,    // DataNode→DataNode dataflow edges
        callsite_args: vec![],  // filled later by post-processing
        cfg_nodes,          // per-function control-flow graph nodes
        cfg_edges,          // per-function control-flow graph edges
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run a query and normalize each capture through the provided function.
fn extract_and_normalize<'a, T>(
    adapter: &dyn LanguageAdapter,
    ts_lang: &tree_sitter::Language,
    query_src: &str,
    root: tree_sitter::Node<'a>,
    source: &str,
    source_bytes: &[u8],
    file_id: FileId,
    file_path: &Path,
    diagnostics: &mut Vec<ExtractDiagnostic>,
    mut normalize: impl FnMut(&dyn LanguageAdapter, String, tree_sitter::Node<'a>, &str, FileId, &Path) -> Option<T>,
) -> Result<Vec<T>> {
    let captures = collect_captures(ts_lang, query_src, root, source_bytes)?;
    let mut results = Vec::new();

    for (name, node) in captures {
        match normalize(adapter, name.clone(), node, source, file_id, file_path) {
            Some(item) => results.push(item),
            None => {
                let pos = node.start_position();
                diagnostics.push(ExtractDiagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!(
                        "Failed to normalize capture '{}' at line {}",
                        name,
                        pos.row + 1
                    ),
                    range: Some(crate::types::TextRange {
                        start_byte: node.start_byte() as u32,
                        end_byte: node.end_byte() as u32,
                        start_line: pos.row as u32,
                        start_column: pos.column as u32,
                        end_line: node.end_position().row as u32,
                        end_column: node.end_position().column as u32,
                    }),
                });
            }
        }
    }

    Ok(results)
}

/// Collect raw (capture_name, node) pairs from a single query.
pub(crate) fn collect_captures<'a>(
    ts_lang: &tree_sitter::Language,
    query_src: &str,
    root: tree_sitter::Node<'a>,
    source_bytes: &[u8],
) -> Result<Vec<(String, tree_sitter::Node<'a>)>> {
    use streaming_iterator::StreamingIterator;

    let trimmed = query_src.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let query = Query::new(ts_lang, trimmed)
        .map_err(|e| anyhow!("Query compile error: {}", e))?;
    let capture_names: Vec<String> = query.capture_names().iter().map(|s| s.to_string()).collect();

    let mut cursor = QueryCursor::new();
    let mut captures_result = Vec::new();

    let mut captures = cursor.captures(&query, root, source_bytes);
    while let Some((m, capture_index)) = captures.next() {
        if let Some(cap) = m.captures.get(*capture_index) {
            let name = capture_names.get(cap.index as usize)
                .cloned()
                .unwrap_or_else(|| format!("capture_{}", cap.index));
            captures_result.push((name, cap.node));
        }
    }
    Ok(captures_result)
}

// ── CFG Helper ────────────────────────────────────────────────────────────

/// Function node kinds that CfgBuilder handles across languages.
const FUNCTION_NODE_KINDS: &[&str] = &[
    "function_declaration", "method_definition", "arrow_function",
    "generator_function_declaration", "generator_function",
    "function_definition", "async_function_definition",
    "method_declaration", "constructor_declaration",
];

/// Build per-function control-flow graphs by matching function symbols
/// to tree-sitter nodes.
fn build_cfg_for_functions<'a>(
    root: tree_sitter::Node<'a>,
    symbols: &[SymbolDef],
    source_bytes: &[u8],
) -> anyhow::Result<CfgResult> {
    let function_symbols: Vec<&SymbolDef> = symbols
        .iter()
        .filter(|s| matches!(s.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor))
        .collect();

    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();

    for sym in &function_symbols {
        if let Some(func_node) = find_function_node(root, sym) {
            let result = CfgBuilder::build(&sym.id, func_node, source_bytes);
            all_nodes.extend(result.nodes);
            all_edges.extend(result.edges);
        }
    }

    Ok(CfgResult { nodes: all_nodes, edges: all_edges })
}

/// Resolve DataNode function_ids by matching each node to its enclosing
/// function symbol.
///
/// For each DataNode with `function_id: None`, finds the function symbol
/// whose range contains the node's start position, and sets the id.
fn resolve_dataflow_function_ids(
    nodes: &mut [DataNode],
    symbols: &[SymbolDef],
) {
    // Build (start_byte, end_byte, symbol_id) for all function symbols
    let function_ranges: Vec<(u32, u32, crate::types::ids::SymbolId)> = symbols
        .iter()
        .filter(|s| matches!(s.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor))
        .map(|s| (s.range.start_byte, s.range.end_byte, s.id))
        .collect();

    if function_ranges.is_empty() {
        return;
    }

    for node in nodes.iter_mut() {
        if node.function_id.is_some() {
            continue;
        }
        // Find the innermost function that contains this node's start position
        let pos = node.range.start_byte;
        let mut best: Option<(u32, u32, crate::types::ids::SymbolId)> = None;
        for (start, end, id) in &function_ranges {
            if pos >= *start && pos <= *end {
                match best {
                    Some((bs, be, _)) if (*end - *start) < (be - bs) => {
                        best = Some((*start, *end, *id));
                    }
                    None => best = Some((*start, *end, *id)),
                    _ => {}
                }
            }
        }
        if let Some((_, _, id)) = best {
            node.function_id = Some(id);
        }
    }
}

/// Walk up from the symbol's name position to find the enclosing function node.
fn find_function_node<'a>(
    root: tree_sitter::Node<'a>,
    symbol: &SymbolDef,
) -> Option<tree_sitter::Node<'a>> {
    let pos = symbol.name_range.start_byte as usize;
    let mut node = root.descendant_for_byte_range(pos, pos)?;
    // Walk up parent chain to find the enclosing function node
    loop {
        if FUNCTION_NODE_KINDS.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::languages::typescript::TypeScriptAdapter;
    use crate::extraction::languages::python::PythonAdapter;
    use crate::types::Language;
    use std::path::PathBuf;

    fn assert_sources_are_known(facts: &FileFacts) {
        let known: std::collections::HashSet<_> = facts.symbols.iter().map(|s| s.id).collect();
        for edge in &facts.raw_edges {
            assert!(known.contains(&edge.source), "raw edge has ghost source: {:?}", edge);
        }
        for callsite in &facts.callsites {
            assert!(known.contains(&callsite.caller), "callsite has ghost caller: {:?}", callsite);
        }
        for reference in &facts.references {
            if let Some(source) = reference.source_symbol {
                assert!(known.contains(&source), "reference has ghost source: {:?}", reference);
            }
        }
    }

    #[test]
    fn test_extract_and_insert_ts_arrow_function_registry_guard() {
        use crate::db::Store;
        let source = r#"export const af = (x: number) => {
  const y = x;
  return y;
};
export function f() {
  return af(1);
}
[1].map(n => n + 1);
"#;
        let file_id = FileId::generate("arrow.ts");
        let adapter = TypeScriptAdapter;
        let file_path = PathBuf::from("arrow.ts");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "javascript")]
    #[test]
    fn test_extract_and_insert_js_arrow_function_registry_guard() {
        use crate::db::Store;
        let source = r#"export const jf = (x) => {
  const y = x;
  return y;
};
function g() {
  return jf(1);
}
"#;
        let file_id = FileId::generate("arrow.js");
        let adapter = crate::extraction::create_adapter(Language::JavaScript).unwrap();
        let file_path = PathBuf::from("arrow.js");

        let facts = extract_file(adapter.as_ref(), file_id, &file_path, source, "abc").unwrap();
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn test_extract_and_insert_cpp_out_of_class_method_registry_guard() {
        use crate::db::Store;
        let source = r#"#include <iostream>
namespace N {
class C {
public:
    void m();
    int field;
};
void C::m() {
    int x = 1;
    std::cout << x;
}
}
"#;
        let file_id = FileId::generate("out_of_class.cpp");
        let adapter = crate::extraction::create_adapter(Language::Cpp).unwrap();
        let file_path = PathBuf::from("out_of_class.cpp");

        let facts = extract_file(adapter.as_ref(), file_id, &file_path, source, "abc").unwrap();
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[test]
    fn test_extract_ts_simple() {
        let source = "const foo = 1;\nconsole.log(foo);\n";
        let file_id = FileId::generate("test.ts");
        let adapter = TypeScriptAdapter;
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.path, "test.ts");
        assert_eq!(facts.file.language, Language::TypeScript);
        assert!(!facts.symbols.is_empty(), "Should have symbols: {:?}", facts.symbols);
        assert!(!facts.references.is_empty(), "Should have references");
    }

    #[test]
    fn test_extract_python_simple() {
        let source = "def foo():\n    return True\n\nfoo()\n";
        let file_id = FileId::generate("test.py");
        let adapter = PythonAdapter;
        let file_path = PathBuf::from("test.py");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.language, Language::Python);
        assert!(!facts.symbols.is_empty(), "Should have symbols");
    }

    #[test]
    fn test_extract_ts_dataflow() {
        // New P3 path: DataFlowBuilder produces DataNodes + DataFlowEdges
        let source = "function add(a: number, b: number) {\n  let result = a + b;\n  return result;\n}\n";
        let file_id = FileId::generate("test.ts");
        let adapter = TypeScriptAdapter;
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.data_nodes.is_empty(), "Should have dataflow nodes");
        assert!(!facts.dataflow_edges.is_empty(), "Should have dataflow edges");
    }

    #[test]
    fn test_extract_python_dataflow() {
        // Python adapter does not yet implement DataFlowBuilder.
        // This test verifies basic extraction still succeeds without the old
        // normalize_dataflow path.
        let source = "def add(a, b):\n    c = a + b\n    return c\n";
        let file_id = FileId::generate("test.py");
        let adapter = PythonAdapter;
        let file_path = PathBuf::from("test.py");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.symbols.is_empty(), "Should have symbols");
        assert!(facts.raw_edges.is_empty(), "Old dataflow path removed");
    }

    #[test]
    fn test_extract_and_insert_ts() {
        use crate::db::Store;
        let source = "function add(a: number, b: number) {\n  return a + b;\n}\nadd(1, 2);\n";
        let file_id = FileId::generate("test.ts");
        let adapter = TypeScriptAdapter;
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        println!("Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!("  sym: {} ({}) qname={} id={}", s.name, s.kind.as_str(), s.qualified_name, &sid[..8]);
        }
        println!("References: {}", facts.references.len());
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!("  edge: {} -> {} ({})", &src[..8], &tgt[..8], e.kind.as_str());
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[test]
    fn test_extract_and_insert_ts_class() {
        use crate::db::Store;
        let source = r#"export class Calculator {
  add(a: number, b: number): number {
    return a + b;
  }
}
const calc = new Calculator();
calc.add(1, 2);
"#;
        let file_id = FileId::generate("test.ts");
        let adapter = TypeScriptAdapter;
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.symbols.is_empty());

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "java")]
    #[test]
    fn test_extract_and_insert_java() {
        use crate::db::Store;
        let source = r#"import java.util.List;

public class UserService {
    private List<String> users;

    public String findById(String id) {
        return users.get(0);
    }

    public void save(String item) {
        users.add(item);
    }
}
"#;
        let file_id = FileId::generate("test.java");
        let adapter = crate::extraction::create_adapter(Language::Java).unwrap();
        let file_path = PathBuf::from("test.java");

        let facts = extract_file(adapter.as_ref(), file_id, &file_path, source, "abc").unwrap();
        println!("Java Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!("  sym: {} ({}) qname={} id={}", s.name, s.kind.as_str(), s.qualified_name, &sid[..8]);
        }
        println!("References: {}", facts.references.len());
        println!("Imports: {}", facts.imports.len());
        println!("Scopes: {}", facts.scopes.len());
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!("  edge: {} -> {} ({})", &src[..8], &tgt[..8], e.kind.as_str());
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "c")]
    #[test]
    fn test_extract_and_insert_c() {
        use crate::db::Store;
        let source = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    char* name;
    char* email;
} User;

User* user_create(const char* name, const char* email) {
    User* u = (User*)malloc(sizeof(User));
    u->name = strdup(name);
    u->email = strdup(email);
    return u;
}

void user_free(User* u) {
    free(u->name);
    free(u->email);
    free(u);
}

char* user_greet(const User* u) {
    char* buf = (char*)malloc(256);
    snprintf(buf, 256, "Hello, %s!", u->name);
    return buf;
}
"#;
        let file_id = FileId::generate("test.c");
        let adapter = crate::extraction::create_adapter(Language::C).unwrap();
        let file_path = PathBuf::from("test.c");

        let facts = extract_file(adapter.as_ref(), file_id, &file_path, source, "abc").unwrap();
        println!("C Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!("  sym: {} ({}) qname={} id={}", s.name, s.kind.as_str(), s.qualified_name, &sid[..8]);
        }
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!("  edge: {} -> {} ({})", &src[..8], &tgt[..8], e.kind.as_str());
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn test_extract_and_insert_cpp() {
        use crate::db::Store;
        let source = r#"#include <iostream>
#include <string>
#include <map>

class UserService {
public:
    std::string findById(const std::string& id) {
        auto it = users_.find(id);
        return it != users_.end() ? it->second : "";
    }

    void save(const std::string& key, const std::string& value) {
        users_[key] = value;
    }

private:
    std::map<std::string, std::string> users_;
};
"#;
        let file_id = FileId::generate("test.cpp");
        let adapter = crate::extraction::create_adapter(Language::Cpp).unwrap();
        let file_path = PathBuf::from("test.cpp");

        let facts = extract_file(adapter.as_ref(), file_id, &file_path, source, "abc").unwrap();
        println!("C++ Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!("  sym: {} ({}) qname={} id={}", s.name, s.kind.as_str(), s.qualified_name, &sid[..8]);
        }
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!("  edge: {} -> {} ({})", &src[..8], &tgt[..8], e.kind.as_str());
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn test_extract_cpp_e2e_fixture() {
        use crate::db::Store;
        let source = r#"#include <iostream>
#include <string>
#include <map>
#include <memory>

class IRepository {
public:
    virtual ~IRepository() = default;
    virtual std::string findById(const std::string& id) = 0;
    virtual void save(const std::string& key, const std::string& value) = 0;
};

class User {
public:
    User(std::string name, std::string email)
        : name_(std::move(name)), email_(std::move(email)) {}

    std::string greet() const {
        return "Hello, " + name_ + "!";
    }

    const std::string& getName() const { return name_; }
    const std::string& getEmail() const { return email_; }

private:
    std::string name_;
    std::string email_;
};

class UserService : public IRepository {
public:
    std::string findById(const std::string& id) override {
        auto it = users_.find(id);
        return it != users_.end() ? it->second : "";
    }

    void save(const std::string& key, const std::string& value) override {
        users_[key] = value;
    }

private:
    std::map<std::string, std::string> users_;
};

int main() {
    auto svc = std::make_unique<UserService>();
    User user("John", "john@example.com");
    svc->save(user.getEmail(), user.getName());
    std::cout << user.greet() << std::endl;
    return 0;
}
"#;
        let file_id = FileId::generate("test.cpp");
        let adapter = crate::extraction::create_adapter(Language::Cpp).unwrap();
        let file_path = PathBuf::from("test.cpp");

        let facts = extract_file(adapter.as_ref(), file_id, &file_path, source, "abc").unwrap();
        println!("C++ E2E Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!("  sym: {} ({}) qname={} id={}", s.name, s.kind.as_str(), s.qualified_name, &sid[..8]);
        }
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!("  edge: {} -> {} ({})", &src[..8], &tgt[..8], e.kind.as_str());
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }
}
