//! QueryEngine: runs tree-sitter queries against source files.
//!
//! Takes a source file (bytes) and queries, parses the source,
//! executes the capture queries, and returns structured query results.
//!
//! tree-sitter 0.25+ bundles its own `StreamingIterator` re-export.

use anyhow::Context;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

/// Raw captures from running all 4 queries against a file.
#[derive(Debug)]
#[allow(dead_code)] // internal test-only engine
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
#[allow(dead_code)] // internal test-only engine
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
    #[allow(dead_code)] // used in tests
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

/// Collects captures from a single query against a tree root.
#[allow(dead_code)]
fn run_one_query(
    ts_lang: &tree_sitter::Language,
    query_src: &str,
    root: tree_sitter::Node,
    source: &[u8],
) -> anyhow::Result<Vec<QueryCapture>> {
    if query_src.trim().is_empty() {
        return Ok(Vec::new());
    }
    let query = Query::new(ts_lang, query_src).context("Failed to compile query")?;
    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut cursor = QueryCursor::new();
    let mut result = Vec::new();

    let mut captures = cursor.captures(&query, root, source);
    while let Some((m, capture_index)) = captures.next() {
        if let Some(cap) = m.captures.get(*capture_index) {
            let name = capture_names
                .get(cap.index as usize)
                .cloned()
                .unwrap_or_else(|| format!("capture_{}", cap.index));
            result.push(QueryCapture::from_ts_capture_node(&name, cap.node, source)?);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{
        ImportExtractorSpec, ParserSpec, ReferenceExtractorSpec, ScopeExtractorSpec,
        SymbolExtractorSpec,
    };
    use tree_sitter::Parser;

    #[cfg(feature = "python")]
    use crate::languages::python::PythonAdapter;
    #[cfg(feature = "typescript")]
    use crate::languages::typescript::TypeScriptFrontendSpec;

    #[cfg(feature = "typescript")]
    fn parse_and_query(spec: &TypeScriptFrontendSpec, source: &str) -> QueryResults {
        let ts_lang = spec.tree_sitter_language();
        let mut parser = Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        QueryResults {
            definitions: run_one_query(&ts_lang, spec.definition_query(), root, bytes).unwrap(),
            references: run_one_query(&ts_lang, spec.reference_query(), root, bytes).unwrap(),
            imports: run_one_query(&ts_lang, spec.import_query(), root, bytes).unwrap(),
            scopes: run_one_query(&ts_lang, spec.scope_query(), root, bytes).unwrap(),
        }
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_query_typescript_simple() {
        let source = "const x: number = 42;\nconsole.log(x);\n";
        let spec = TypeScriptFrontendSpec;
        let results = parse_and_query(&spec, source);
        assert!(
            !results.definitions.is_empty(),
            "Expected at least one definition"
        );
        assert!(
            !results.references.is_empty(),
            "Expected at least one reference"
        );
    }

    #[cfg(feature = "python")]
    #[test]
    fn test_query_python_simple() {
        let source = "def foo():\n    return True\n\nfoo()\n";
        let spec = PythonAdapter;
        let ts_lang = spec.tree_sitter_language();
        let mut parser = Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let results = QueryResults {
            definitions: run_one_query(&ts_lang, spec.definition_query(), root, bytes).unwrap(),
            references: run_one_query(&ts_lang, spec.reference_query(), root, bytes).unwrap(),
            imports: run_one_query(&ts_lang, spec.import_query(), root, bytes).unwrap(),
            scopes: run_one_query(&ts_lang, spec.scope_query(), root, bytes).unwrap(),
        };
        assert!(
            !results.definitions.is_empty(),
            "Expected at least one definition"
        );
        assert!(
            !results.references.is_empty(),
            "Expected at least one reference"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_query_empty_source() {
        let source = "";
        let spec = TypeScriptFrontendSpec;
        let results = parse_and_query(&spec, source);
        assert!(results.definitions.is_empty());
        assert!(results.references.is_empty());
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn test_query_capture_has_positions() {
        let source = "let foo = 1;\n";
        let spec = TypeScriptFrontendSpec;
        let results = parse_and_query(&spec, source);
        for cap in &results.definitions {
            assert!(!cap.text.is_empty(), "capture text must not be empty");
            assert!(cap.end_byte > cap.start_byte, "invalid byte range");
        }
    }
}
