use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::{FileId, SourceExtractor, Store, SymbolId};

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
