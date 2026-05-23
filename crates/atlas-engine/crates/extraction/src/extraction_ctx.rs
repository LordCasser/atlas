//! Per-file extraction context shared by every domain-level extraction helper.
//!
//! `ExtractionCtx` bundles tree-sitter parser state, source text, and file
//! identity so extraction helpers (`extract_and_normalize`, `LexicalBinder`,
//! `DataFlowBuilder`, `build_reference_binding_uses`) take one struct instead
//! of 7-9 individual parameters.

use std::path::Path;

use types::{FileId, Language};

use crate::frontend::NormalizeCtx;

/// Per-file extraction context shared by every domain-level extraction helper.
pub(crate) struct ExtractionCtx<'a> {
    pub ts_lang: &'a tree_sitter::Language,
    pub root: tree_sitter::Node<'a>,
    pub source: &'a str,
    pub file_id: FileId,
    pub file_path: &'a Path,
    pub language: Language,
}

impl<'a> ExtractionCtx<'a> {
    /// Source bytes for tree-sitter query execution.
    pub fn source_bytes(&self) -> &[u8] {
        self.source.as_bytes()
    }

    /// Build a `NormalizeCtx` (subset used by slot normalize methods).
    pub fn normalize_ctx(&self) -> NormalizeCtx<'a> {
        NormalizeCtx {
            language: self.language,
            file_id: self.file_id,
            file_path: self.file_path,
            source: self.source,
        }
    }
}
