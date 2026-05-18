//! QueryEngine: runs tree-sitter queries against source files.
//!
//! Takes a source file (bytes) and a LanguageAdapter, parses the source,
//! executes the 4 capture queries, and returns structured query results.

use anyhow::Context;
use tree_sitter::{Parser, Query, QueryCursor};
use streaming_iterator::StreamingIterator;

use crate::extraction::languages::LanguageAdapter;

/// Raw captures from running all 4 queries against a file.
#[derive(Debug)]
pub struct QueryResults {
    /// Captures from the `definition_query`.
    pub definitions: Vec<QueryCapture>,
    /// Captures from the `reference_query`.
    pub references: Vec<QueryCapture>,
    /// Captures from the `import_query`.
    pub imports: Vec<QueryCapture>,
    /// Captures from the `scope_query`.
    pub scopes: Vec<QueryCapture>,
}

/// A single tree-sitter capture: a named node matched by a query pattern.
#[derive(Debug, Clone)]
pub struct QueryCapture {
    /// The capture name as defined in the query (e.g. "definition.function").
    pub capture_name: String,
    /// The source text covered by the capture node.
    pub text: String,
    /// Byte range in the source file.
    pub start_byte: u32,
    pub end_byte: u32,
    /// Line/column position (0-based line, 0-based UTF-16 column in tree-sitter).
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl QueryCapture {
    /// Build a QueryCapture from a capture name and a tree-sitter node.
    fn from_ts_capture_node(
        capture_name: &str,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> anyhow::Result<Self> {
        let text = node
            .utf8_text(source)
            .context("Failed to read node text")?
            .to_string();

        let start = node.start_position();
        let end = node.end_position();

        Ok(QueryCapture {
            capture_name: capture_name.to_string(),
            text,
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            start_line: start.row as u32,
            start_column: start.column as u32,
            end_line: end.row as u32,
            end_column: end.column as u32,
        })
    }
}

/// Runs the 4 query files (definitions, references, imports, scopes) provided
/// by a language adapter against `source` and collects all captures.
pub fn run_queries(
    adapter: &dyn LanguageAdapter,
    source: &[u8],
) -> anyhow::Result<QueryResults> {
    let ts_lang = adapter.tree_sitter_language();

    // 1. Parse
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .context("Failed to set tree-sitter language")?;

    let tree = parser
        .parse(source, None)
        .context("Failed to parse source")?;

    let root = tree.root_node();

    if root.has_error() {
        // Parse errors are non-fatal: we continue and extract what we can.
        // Diagnostics are collected later by the caller.
    }

    // 2. Helper: compile and run a single query.
    let run_one = |query_src: &str| -> anyhow::Result<Vec<QueryCapture>> {
        if query_src.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query = Query::new(&ts_lang, query_src)
            .context("Failed to compile query")?;
        let capture_names: Vec<String> = query.capture_names().iter().map(|s| s.to_string()).collect();

        let mut cursor = QueryCursor::new();
        let mut result = Vec::new();

        // tree-sitter 0.24: captures() returns QueryCaptures which implements StreamingIterator
        let mut captures = cursor.captures(&query, root, source);
        while let Some((m, capture_index)) = captures.next() {
            if let Some(cap) = m.captures.get(*capture_index) {
                let name = capture_names.get(cap.index as usize)
                    .cloned()
                    .unwrap_or_else(|| format!("capture_{}", cap.index));
                result.push(QueryCapture::from_ts_capture_node(
                    &name,
                    cap.node,
                    source,
                )?);
            }
        }
        Ok(result)
    };

    // 3. Run all 4 queries
    let definitions = run_one(adapter.definition_query())?;
    let references = run_one(adapter.reference_query())?;
    let imports = run_one(adapter.import_query())?;
    let scopes = run_one(adapter.scope_query())?;

    Ok(QueryResults {
        definitions,
        references,
        imports,
        scopes,
    })
}

/// Convenience: run all queries directly on raw source text (UTF-8).
pub fn run_queries_text(
    adapter: &dyn LanguageAdapter,
    source_text: &str,
) -> anyhow::Result<QueryResults> {
    run_queries(adapter, source_text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::languages::typescript::TypeScriptAdapter;
    use crate::extraction::languages::python::PythonAdapter;

    #[test]
    fn test_query_typescript_simple() {
        let source = "const x: number = 42;\nconsole.log(x);\n";
        let adapter = TypeScriptAdapter;
        let results = run_queries_text(&adapter, source).unwrap();
        // We should have at least one definition capture
        assert!(!results.definitions.is_empty(),
            "Expected at least one definition");
        // Verify at least one reference capture
        assert!(!results.references.is_empty(),
            "Expected at least one reference");
    }

    #[test]
    fn test_query_python_simple() {
        let source = "def foo():\n    return True\n\nfoo()\n";
        let adapter = PythonAdapter;
        let results = run_queries_text(&adapter, source).unwrap();
        assert!(!results.definitions.is_empty(),
            "Expected at least one definition");
        assert!(!results.references.is_empty(),
            "Expected at least one reference");
    }

    #[test]
    fn test_query_empty_source() {
        let source = "";
        let adapter = TypeScriptAdapter;
        let results = run_queries_text(&adapter, source).unwrap();
        assert!(results.definitions.is_empty());
        assert!(results.references.is_empty());
    }

    #[test]
    fn test_query_capture_has_positions() {
        let source = "let foo = 1;\n";
        let adapter = TypeScriptAdapter;
        let results = run_queries_text(&adapter, source).unwrap();
        for cap in &results.definitions {
            // All captures should have valid text and range
            assert!(!cap.text.is_empty(), "capture text must not be empty");
            assert!(cap.end_byte > cap.start_byte, "invalid byte range");
        }
    }
}
