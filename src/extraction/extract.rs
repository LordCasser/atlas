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
    DiagnosticLevel, ExtractDiagnostic, FileFacts, FileInfo,
    ParseStatus,
};
use crate::types::ids::FileId;

use super::languages::LanguageAdapter;

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
    let symbols = extract_and_normalize(
        adapter, &ts_lang, adapter.definition_query(), root, source, source_bytes,
        file_id, file_path, &mut diagnostics,
        |adapter, name, node, src, fid, fp| {
            adapter.normalize_definition(&name, node, src, fid, fp)
        },
    )?;

    // 3. Extract and normalize references
    let references = extract_and_normalize(
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
    let scopes = extract_and_normalize(
        adapter, &ts_lang, adapter.scope_query(), root, source, source_bytes,
        file_id, file_path, &mut diagnostics,
        |adapter, name, node, src, fid, fp| {
            adapter.normalize_scope(&name, node, src, fid, fp)
        },
    )?;

    // 6. Extract and normalize dataflow edges (parameter, returns, assignments, field access)
    let dataflow_query = adapter.dataflow_query();
    let raw_edges = if dataflow_query.trim().is_empty() {
        Vec::new()
    } else {
        extract_and_normalize(
            adapter, &ts_lang, dataflow_query, root, source, source_bytes,
            file_id, file_path, &mut diagnostics,
            |adapter, name, node, src, fid, fp| {
                adapter.normalize_dataflow(&name, node, src, fid, fp)
            },
        )?
    };

    // 7. (reserved for callsite extraction in future milestone)

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
        exports: Vec::new(),
        raw_edges,   // Dataflow edges extracted inline; structural edges still by resolver
        callsites: Vec::new(),   // Callsites derived from call references later
        diagnostics,
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
fn collect_captures<'a>(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::languages::typescript::TypeScriptAdapter;
    use crate::extraction::languages::python::PythonAdapter;
    use crate::types::Language;
    use std::path::PathBuf;

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
        let source = "function add(a: number, b: number) {\n  let result = a + b;\n  return result;\n}\n";
        let file_id = FileId::generate("test.ts");
        let adapter = TypeScriptAdapter;
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.raw_edges.is_empty(), "Should have dataflow edges");
        // Check for at least one parameter edge
        let has_param = facts.raw_edges.iter().any(|e| e.kind.as_str() == "parameter");
        assert!(has_param, "Expected parameter edges, got: {:?}", facts.raw_edges.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>());
    }

    #[test]
    fn test_extract_python_dataflow() {
        let source = "def add(a, b):\n    c = a + b\n    return c\n";
        let file_id = FileId::generate("test.py");
        let adapter = PythonAdapter;
        let file_path = PathBuf::from("test.py");

        let facts = extract_file(&adapter, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.raw_edges.is_empty(), "Should have dataflow edges");
        // Check for at least one parameter edge
        let has_param = facts.raw_edges.iter().any(|e| e.kind.as_str() == "parameter");
        assert!(has_param, "Expected parameter edges, got: {:?}", facts.raw_edges.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>());
    }
}
