//! Store query runtime — direct store reads + source extraction.
//!
//! # Responsibilities
//! - File path resolution from FileId
//! - Source code extraction via tree-sitter AST
//! - Capability statistics querying
//! - Not-indexed guidance message
//!
//! # Usage pattern
//! ```ignore
//! let path = self.active.store_query_runtime.resolve_file_path(&file_id);
//! let source = self.active.store_query_runtime.read_symbol_source(&symbol_id);
//! ```
//!
//! # Dependencies
//! - `atlas_engine::{Store, SourceExtractor, FileId, SymbolId}`

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::{FileId, SourceExtractor, Store, SymbolId};

/// Direct store-fact queries (symbols, files, usages) that don't
/// require the full in-memory graph or focus-driven extraction.
pub struct StoreQueryRuntime {
    pub store: Arc<Store>,
    pub source_extractor: SourceExtractor,
}

impl StoreQueryRuntime {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        let source_extractor = SourceExtractor::new(store.clone(), project_root);
        Self {
            store,
            source_extractor,
        }
    }

    /// Resolve a [`FileId`] to its human-readable file path.
    /// Falls back to the hex representation if the file is not found.
    pub fn resolve_file_path(&self, file_id: &FileId) -> String {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|f| f.path)
            .unwrap_or_else(|| file_id.to_hex())
    }

    /// Read source code for a symbol using AST-aware extraction.
    ///
    /// Delegates to [`SourceExtractor`] which re-parses the file with
    /// tree-sitter and extracts the exact definition-node source text.
    /// Falls back to `TextRange`-based line extraction when tree-sitter
    /// parsing is unavailable.
    ///
    /// Returns `None` if the file cannot be found, is outside the project
    /// root, or the symbol range is invalid.  Callers should silently omit
    /// the `source` field when this returns `None`.
    pub fn read_symbol_source(&self, symbol_id: &SymbolId) -> Option<String> {
        self.source_extractor.extract_source(symbol_id)
    }

    /// Return a guidance hint when the project has no materialized files.
    ///
    /// Returns a user-facing guidance string suggesting scoped focus queries
    /// when the store has no materialized files, or an empty string otherwise.
    pub fn not_indexed_guidance(&self) -> &'static str {
        if self.store.count_files().unwrap_or(0) == 0 {
            "\nHint: No project facts have been materialized in this MCP store yet. Start with a scoped search or provide a file_path/scope so focus can extract the relevant local code. For explicit project-wide indexing, use the CLI `atlas index` command outside MCP."
        } else {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{FileId, Store};
    use std::sync::Arc;

    fn create_test_store_query_runtime() -> StoreQueryRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        StoreQueryRuntime::new(store, PathBuf::from("/test/project"))
    }

    #[test]
    fn resolve_file_path_returns_hex_for_unknown_file() {
        let sqr = create_test_store_query_runtime();
        // Generate a FileId for a file that does not exist in the store.
        let file_id = FileId::generate("nonexistent.rs");
        let result = sqr.resolve_file_path(&file_id);
        // Should fall back to the hex representation of the blake3 hash.
        assert!(!result.is_empty());
        assert_eq!(result.len(), 64);
    }
}
