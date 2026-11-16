//! Shared tree-sitter query helpers for the extraction layer.
//!
//! Contains utilities shared between `extract.rs` and other extraction modules
//! (lexical_binder, dataflow_builder, etc.) that need to run tree-sitter queries
//! independent of the main extractor pipeline.
//!
//! tree-sitter 0.25+ bundles its own `StreamingIterator` re-export instead of
//! requiring the external `streaming_iterator` crate.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use tracing::debug_span;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::cancel::CancelCheck;
use crate::error::{ExtractionFailure, ExtractionFailureKind};

// Programs depend on the actual grammar and query text, never on a source file
// or its normalized annotations. Cursors and captures remain invocation-local.
// Only retain a bounded number of small query sources; larger custom programs
// still compile normally, without occupying this process-wide cache.
const MAX_CACHED_QUERIES: usize = 32;
const MAX_CACHED_QUERY_SOURCE_BYTES: usize = 64 * 1024;

struct CachedQuery {
    language: tree_sitter::Language,
    source: Box<str>,
    program: Arc<Query>,
}

static QUERY_CACHE: OnceLock<Mutex<VecDeque<CachedQuery>>> = OnceLock::new();

fn compiled_query(
    language: &tree_sitter::Language,
    source: &str,
) -> Result<Arc<Query>, tree_sitter::QueryError> {
    if source.len() > MAX_CACHED_QUERY_SOURCE_BYTES {
        return Query::new(language, source).map(Arc::new);
    }
    let cache = QUERY_CACHE.get_or_init(|| Mutex::new(VecDeque::new()));
    {
        let entries = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = entries
            .iter()
            .find(|entry| &entry.language == language && entry.source.as_ref() == source)
        {
            return Ok(Arc::clone(&entry.program));
        }
    }

    // Compile outside the mutex so unrelated grammars and cache hits are not
    // held behind a cold compilation. Concurrent misses may compile twice.
    let program = Arc::new(Query::new(language, source)?);
    let mut entries = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = entries
        .iter()
        .find(|entry| &entry.language == language && entry.source.as_ref() == source)
    {
        return Ok(Arc::clone(&entry.program));
    }
    if entries.len() == MAX_CACHED_QUERIES {
        entries.pop_front();
    }
    entries.push_back(CachedQuery {
        language: language.clone(),
        source: source.into(),
        program: Arc::clone(&program),
    });
    Ok(program)
}

/// Collect raw (capture_name, node) pairs from a single query.
///
/// `slot` identifies the query that failed (e.g. `"symbols"`, `"lexical"`)
/// and is attached to the typed [`ExtractionFailure`] on error so the worker
/// can report which query phase triggered the failure.
/// If `cancel_token` is set, cancellation returns
/// [`ExtractionFailureKind::Cancelled`] instead of partial captures.
pub(crate) fn collect_captures<'a>(
    ts_lang: &tree_sitter::Language,
    query_src: &str,
    root: Node<'a>,
    source_bytes: &[u8],
    slot: &'static str,
    cancel_token: Option<&dyn CancelCheck>,
) -> Result<Vec<(String, Node<'a>)>, ExtractionFailure> {
    let trimmed = query_src.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let query = {
        let _query_span =
            debug_span!(target: "atlas_extract", "extract.query_prepare", slot = slot).entered();
        match compiled_query(ts_lang, trimmed) {
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
        }
    };

    let capture_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut cursor = QueryCursor::new();
    let mut captures_result = Vec::new();
    let mut seen = HashSet::new();

    let mut captures = cursor.captures(&query, root, source_bytes);
    let mut count = 0usize;
    while let Some((m, capture_index)) = captures.next() {
        if let Some(cap) = m.captures.get(*capture_index)
            && seen.insert((cap.index, cap.node.id()))
        {
            let name = capture_names
                .get(cap.index as usize)
                .cloned()
                .unwrap_or_else(|| format!("capture_{}", cap.index));
            // Captures prefixed with `_` exist only for query predicates.
            // They are constraints, not facts for language normalizers.
            if !name.starts_with('_') {
                captures_result.push((name, cap.node));
            }
        }
        count += 1;
        if count.is_multiple_of(100)
            && let Some(t) = cancel_token
            && t.is_cancelled()
        {
            return Err(ExtractionFailure {
                kind: ExtractionFailureKind::Cancelled,
                file_path: String::new(), // caller fills if needed
                language: types::Language::TypeScript, // placeholder — caller fills
                slot: Some(slot),
                message: "cancelled".to_string(),
            });
        }
    }
    Ok(captures_result)
}

#[cfg(all(test, feature = "cpp"))]
mod tests {
    use super::*;

    fn cpp() -> tree_sitter::Language {
        crate::create_frontend(types::Language::Cpp)
            .unwrap()
            .parser
            .tree_sitter_language()
    }

    fn captures(
        language: &tree_sitter::Language,
        query: &str,
        source: &str,
    ) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        collect_captures(
            language,
            query,
            tree.root_node(),
            source.as_bytes(),
            "test",
            None,
        )
        .unwrap()
        .into_iter()
        .map(|(name, node)| (name, node.utf8_text(source.as_bytes()).unwrap().to_owned()))
        .collect()
    }

    #[test]
    fn repeated_programs_keep_sources_and_query_text_independent() {
        let language = cpp();
        let names = "(identifier) @name";
        assert_eq!(
            captures(&language, names, "int first;"),
            [("name".into(), "first".into())]
        );
        assert_eq!(
            captures(&language, names, "int second;"),
            [("name".into(), "second".into())]
        );
        assert_eq!(
            captures(&language, "(primitive_type) @type", "int second;"),
            [("type".into(), "int".into())]
        );
        assert_eq!(
            captures(&language, "(identifier) @_constraint", "int second;"),
            []
        );
        assert_eq!(
            captures(&language, names, "int first;"),
            [("name".into(), "first".into())]
        );
        assert!(compiled_query(&language, "(not_a_cpp_node) @invalid").is_err());
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn a_program_from_another_grammar_cannot_mask_a_compile_error() {
        let source = "(destructor_name) @name";
        compiled_query(&cpp(), source).unwrap();
        let typescript = crate::create_frontend(types::Language::TypeScript)
            .unwrap()
            .parser
            .tree_sitter_language();
        assert!(compiled_query(&typescript, source).is_err());
        assert!(compiled_query(&typescript, source).is_err());
        assert!(compiled_query(&cpp(), source).is_ok());
    }

    #[test]
    fn a_warm_program_preserves_capture_cancellation() {
        struct Canceled;
        impl CancelCheck for Canceled {
            fn is_cancelled(&self) -> bool {
                true
            }
        }
        let language = cpp();
        let query = "(identifier) @name";
        assert_eq!(captures(&language, query, "int seed;").len(), 1);
        let source: String = (0..200).map(|n| format!("int value{n};\n")).collect();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(&source, None).unwrap();
        let error = collect_captures(
            &language,
            query,
            tree.root_node(),
            source.as_bytes(),
            "test",
            Some(&Canceled),
        )
        .unwrap_err();
        assert!(matches!(error.kind, ExtractionFailureKind::Cancelled));
    }
}
