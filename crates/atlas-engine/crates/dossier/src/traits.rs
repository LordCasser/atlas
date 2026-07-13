//! Repository traits for the Symbol Dossier.
//!
//! These traits abstract the data sources needed to build a dossier:
//! symbol resolution, relation evidence, file facts, and source text.
//! Implementations are deferred to Phase 2.

use anyhow::Result;

/// A single relation occurrence with its evidence.
///
/// The `range` field captures the source location where this relation is
/// evidenced (e.g., the call-site expression). The snippet is filled in by
/// the dossier builder when requested.
#[derive(Debug, Clone)]
pub struct RelationEvidence {
    /// Source symbol of the relation.
    pub source_id: types::SymbolId,
    /// Target symbol of the relation.
    pub target_id: types::SymbolId,
    /// What kind of relation this is.
    pub relation_kind: super::types::InternalRelationKind,
    /// File where this relation is evidenced.
    pub file_id: types::FileId,
    /// Range in source where this relation is evidenced (e.g., the callsite expression).
    pub range: types::TextRange,
    /// Confidence in this relation.
    pub confidence: types::enums::Confidence,
}

// ---------------------------------------------------------------------------
// SymbolRepository
// ---------------------------------------------------------------------------

/// Resolves symbol query strings to symbol definitions.
pub trait SymbolRepository {
    /// Resolve a symbol query string to one or more symbols.
    ///
    /// Returns an empty `Vec` if no match is found.
    /// May return multiple results for ambiguous queries (Decision #4).
    fn resolve(&self, query: &str) -> Result<Vec<types::SymbolDef>>;

    /// Get the compact declaration signature for a symbol.
    ///
    /// Returns `None` when the language adapter cannot derive a useful shape.
    fn get_signature(&self, symbol_id: &types::SymbolId) -> Result<Option<String>>;

    /// Get symbol definition by ID.
    ///
    /// Returns `None` if the symbol is not found.
    fn get_symbol_by_id(&self, id: &types::SymbolId) -> Result<Option<types::SymbolDef>>;

    /// Get the project-relative file path for a `FileId`.
    ///
    /// Returns `None` if the file is unknown.
    fn get_file_path(&self, file_id: &types::FileId) -> Result<Option<String>>;
}

// ---------------------------------------------------------------------------
// RelationRepository
// ---------------------------------------------------------------------------

/// Provides relation evidence (edges with source locations) for a symbol.
pub trait RelationRepository {
    /// Get incoming relations for a symbol, with evidence.
    ///
    /// If `kinds` is `None`, returns all supported kinds.
    /// Results are limited to `limit` examples per kind group.
    fn incoming_evidence(
        &self,
        symbol_id: &types::SymbolId,
        kinds: Option<&[super::types::InternalRelationKind]>,
        limit: usize,
    ) -> Result<Vec<RelationEvidence>>;

    /// Get outgoing relations for a symbol, with evidence.
    fn outgoing_evidence(
        &self,
        symbol_id: &types::SymbolId,
        kinds: Option<&[super::types::InternalRelationKind]>,
        limit: usize,
    ) -> Result<Vec<RelationEvidence>>;

    /// Count incoming relations, grouped by `InternalRelationKind`.
    fn count_incoming_by_kind(
        &self,
        symbol_id: &types::SymbolId,
    ) -> Result<std::collections::HashMap<super::types::InternalRelationKind, usize>>;

    /// Count outgoing relations, grouped by `InternalRelationKind`.
    fn count_outgoing_by_kind(
        &self,
        symbol_id: &types::SymbolId,
    ) -> Result<std::collections::HashMap<super::types::InternalRelationKind, usize>>;
}

// ---------------------------------------------------------------------------
// FileFactsRepository
// ---------------------------------------------------------------------------

/// Provides file-level facts: imports, exports, and peer symbols.
pub trait FileFactsRepository {
    /// Get import statements for a file.
    fn get_imports(&self, file_id: &types::FileId) -> Result<Vec<types::ImportDef>>;

    /// Get export declarations for a file.
    ///
    /// When export data is unavailable (e.g., unsupported language), returns
    /// an empty `Vec` — no visibility fallback per Decision #1.
    fn get_exports(&self, file_id: &types::FileId) -> Result<Vec<super::types::ExportFact>>;

    /// Get peer symbols in the same file, excluding the given symbol.
    ///
    /// Results are limited to `limit` symbols. The `exclude_id` is filtered out
    /// to avoid the subject appearing in its own peer list.
    fn get_peers(
        &self,
        file_id: &types::FileId,
        exclude_id: &types::SymbolId,
        limit: usize,
    ) -> Result<Vec<types::SymbolDef>>;
}

// ---------------------------------------------------------------------------
// SourceRepository
// ---------------------------------------------------------------------------

/// Reads source text from disk, with per-request caching (Decision #2).
///
/// Implementations MUST cache file content per-request to avoid repeated I/O.
/// The cache is explicitly released via `clear_cache()` after dossier build
/// completes.
pub trait SourceRepository {
    /// Read source text for a given byte or line range in a file.
    fn read_range(&self, file_id: &types::FileId, range: &types::TextRange) -> Result<String>;

    /// Read a range of lines from a file.
    ///
    /// `start_line` and `end_line` are 1-based inclusive.
    fn read_lines(&self, file_id: &types::FileId, start_line: u32, end_line: u32)
    -> Result<String>;

    /// Release per-request cache.
    ///
    /// Called after dossier build completes to free memory.
    fn clear_cache(&self);
}
