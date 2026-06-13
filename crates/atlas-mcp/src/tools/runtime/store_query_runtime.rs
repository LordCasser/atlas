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

use crate::tools::lazy_response::CapabilityStats;

/// Direct store-fact queries (symbols, files, usages) that don't
/// require the full in-memory graph or focus-driven extraction.
pub struct StoreQueryRuntime {
    pub store: Arc<Store>,
    pub source_extractor: SourceExtractor,
    /// Root directory of the project. Stored for future path-resolution helpers.
    #[allow(dead_code)]
    pub project_root: PathBuf,
}

impl StoreQueryRuntime {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        let source_extractor = SourceExtractor::new(store.clone(), project_root.clone());
        Self {
            store,
            source_extractor,
            project_root,
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

    /// Return a guidance hint when the project has no indexed files.
    ///
    /// Returns a user-facing guidance string suggesting the `index` tool
    /// when the store has no indexed files, or an empty string otherwise.
    pub fn not_indexed_guidance(&self) -> &'static str {
        if self.store.count_files().unwrap_or(0) == 0 {
            "\nHint: The project has not been indexed yet. Please run the 'index' tool first (fast manifest indexing) to build the code index, then retry this query."
        } else {
            ""
        }
    }

    /// Query the DB for real capability file counts.
    /// Returns None if the query fails (graceful degradation).
    /// Reserved for future status/capabilities reporting endpoints.
    #[allow(dead_code)]
    pub fn get_capability_stats(&self) -> Option<CapabilityStats> {
        let (files_with_dataflow, files_structural_only, files_manifest_only, files_with_cfg) =
            self.store.get_capability_counts().ok()?;
        Some(CapabilityStats {
            files_with_dataflow,
            files_structural_only,
            files_manifest_only,
            files_with_cfg,
        })
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

    #[test]
    fn project_root_is_accessible() {
        let sqr = create_test_store_query_runtime();
        // Prove the project_root field is wired correctly — reserved for
        // future path-resolution helpers.
        assert_eq!(sqr.project_root, PathBuf::from("/test/project"));
    }
}
