//! MCP wrappers for the engine-layer [`atlas_engine::symbol_selector`] module.
//!
//! This file provides:
//! 1. Re-exports of all engine types for MCP tool handlers.
//! 2. Convenience methods on [`ToolRouter`] that delegate to engine functions.
//!
//! The core resolution logic lives in [`atlas_engine::symbol_selector`].

// Re-export engine types for internal MCP use
pub(crate) use atlas_engine::symbol_selector::{
    MatchInfo, MatchMode, PathMatchQuality, ResolvedSymbol, ScoredCandidate,
    SymbolInput, SymbolResolution, SymbolResolutionPolicy, SymbolSelector,
    compute_ignored_mismatches, find_similar_names, normalize_and_validate_path,
    MAX_AGGREGATION_CANDIDATES,
};

use super::ToolRouter;

impl ToolRouter {
    /// Unified symbol resolution — delegates to engine.
    pub(crate) fn resolve_symbol_input(
        &self,
        input: &SymbolInput,
        policy: SymbolResolutionPolicy,
    ) -> Result<SymbolResolution, String> {
        atlas_engine::symbol_selector::resolve_symbol_input(&self.store, input, policy)
    }

    /// Resolve a file_id to a project-relative path string.
    /// Used by context/graph tools that need to construct path info.
    pub(crate) fn resolve_file_path_for_id(&self, file_id: &atlas_engine::FileId) -> String {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|f| f.path)
            .unwrap_or_default()
    }
}
