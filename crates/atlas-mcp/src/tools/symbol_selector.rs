//! MCP wrappers for the engine-layer [`atlas_engine::symbol_selector`] module.
//!
//! This file provides:
//! 1. Re-exports of all engine types for MCP tool handlers.
//! 2. Convenience methods on [`ToolRouter`] that delegate to engine functions.
//!
//! The core resolution logic lives in [`atlas_engine::symbol_selector`].

// Re-export engine types for internal MCP use
pub(crate) use atlas_engine::symbol_selector::{
    ResolvedSymbol, ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
    SymbolSelector, MAX_AGGREGATION_CANDIDATES,
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

}
