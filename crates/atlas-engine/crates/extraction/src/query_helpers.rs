//! Shared tree-sitter query helpers for the extraction layer.
//!
//! Contains utilities shared between `extract.rs` and other extraction modules
//! (lexical_binder, dataflow_builder, etc.) that need to run tree-sitter queries
//! independent of the main extractor pipeline.
//!
//! tree-sitter 0.25+ bundles its own `StreamingIterator` re-export instead of
//! requiring the external `streaming_iterator` crate.

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::error::{ExtractionFailure, ExtractionFailureKind};

/// Collect raw (capture_name, node) pairs from a single query.
///
/// `slot` identifies the query that failed (e.g. `"symbols"`, `"lexical"`)
/// and is attached to the typed [`ExtractionFailure`] on error so the worker
/// can report which query phase triggered the failure.
pub(crate) fn collect_captures<'a>(
    ts_lang: &tree_sitter::Language,
    query_src: &str,
    root: Node<'a>,
    source_bytes: &[u8],
    slot: &'static str,
) -> Result<Vec<(String, Node<'a>)>, ExtractionFailure> {
    let trimmed = query_src.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let query = match Query::new(ts_lang, trimmed) {
        Ok(q) => q,
        Err(e) => {
            return Err(ExtractionFailure {
                kind: ExtractionFailureKind::QueryCompile,
                file_path: String::new(), // caller fills if needed
                language: types::Language::TypeScript, // placeholder — caller fills
                slot: Some(slot),
                message: format!("{e}"),
            });
        }
    };

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
