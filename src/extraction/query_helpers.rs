//! Shared tree-sitter query helpers for the extraction layer.
//!
//! Contains utilities shared between `extract.rs` and other extraction modules
//! (lexical_binder, dataflow_builder, etc.) that need to run tree-sitter queries
//! independent of the main extractor pipeline.

use anyhow::{Result, anyhow};
use tree_sitter::{Node, Query, QueryCursor};

/// Collect raw (capture_name, node) pairs from a single query.
pub(crate) fn collect_captures<'a>(
    ts_lang: &tree_sitter::Language,
    query_src: &str,
    root: Node<'a>,
    source_bytes: &[u8],
) -> Result<Vec<(String, Node<'a>)>> {
    use streaming_iterator::StreamingIterator;

    let trimmed = query_src.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let query = Query::new(ts_lang, trimmed).map_err(|e| anyhow!("Query compile error: {}", e))?;
    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut cursor = QueryCursor::new();
    let mut captures_result = Vec::new();

    let mut captures = cursor.captures(&query, root, source_bytes);
    while let Some((m, capture_index)) = captures.next() {
        if let Some(cap) = m.captures.get(*capture_index) {
            let name = capture_names
                .get(cap.index as usize)
                .cloned()
                .unwrap_or_else(|| format!("capture_{}", cap.index));
            captures_result.push((name, cap.node));
        }
    }
    Ok(captures_result)
}
