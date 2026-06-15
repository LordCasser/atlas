//! Analysis runtime — on-demand CFG/dataflow extraction.
//!
//! # Responsibilities
//! - Single entry point (`ensure_dataflow_for_function`) for lazy CFG extraction
//! - Wraps LazyDataflowService — no other module calls it directly
//!
//! # Usage pattern
//! ```ignore
//! let window = self.active.analysis_runtime.ensure_dataflow_for_function(&symbol_id, Some(&query_id))?;
//! ```
//!
//! # Dependencies
//! - `atlas_engine::LazyDataflowService`

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::{CfgEdge, CfgNode, LazyDataflowService, LazyWindow, Store, SymbolId};

/// Provides CFG and dataflow facts for branch_diff and lifecycle analysis.
///
/// In Phase 3, this will become the single entry point for triggering
/// lazy dataflow extraction, replacing the current ad-hoc pattern where
/// handlers call `lazy_service.ensure_for_function()` directly.
pub struct AnalysisRuntime {
    pub lazy_service: LazyDataflowService,
}

impl AnalysisRuntime {
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        let lazy_service = LazyDataflowService::new(store, project_root);
        Self { lazy_service }
    }

    /// Trigger lazy dataflow extraction for a function symbol.
    ///
    /// Triggers on-demand CFG + dataflow build and returns the lazy window
    /// that describes the local analysis boundary.
    pub fn ensure_dataflow_for_function(
        &self,
        symbol_id: &SymbolId,
        query_id: Option<&str>,
    ) -> anyhow::Result<LazyWindow> {
        self.lazy_service.ensure_for_function(symbol_id, query_id)
    }

    /// Ensure CFG nodes and edges are available for a function, with lazy fallback.
    ///
    /// Queries the store for CFG nodes. If none exist, triggers lazy CFG extraction
    /// via [`ensure_dataflow_for_function`] and re-queries. Returns `Err(String)` with
    /// a human-readable message when the CFG still cannot be loaded.
    pub fn ensure_cfg_for_function(
        &self,
        store: &Store,
        sid: &SymbolId,
        query_id: &str,
        fn_name: &str,
    ) -> Result<(Vec<CfgNode>, Vec<CfgEdge>), String> {
        let mut cfg_nodes = store
            .find_cfg_nodes_by_function(sid)
            .map_err(|e| format!("Failed to load CFG nodes: {e}"))?;

        if cfg_nodes.is_empty() {
            // Trigger lazy CFG extraction
            self.ensure_dataflow_for_function(sid, Some(query_id))
                .map_err(|e| format!("CFG not available for analysis of '{fn_name}': {e:#}"))?;
            // Re-query after lazy extraction
            cfg_nodes = store
                .find_cfg_nodes_by_function(sid)
                .map_err(|e| format!("Failed to load CFG nodes after lazy extraction: {e}"))?;
        }

        if cfg_nodes.is_empty() {
            return Err(format!(
                "CFG not available for '{fn_name}'. The function may be in a language that does not yet support CFG extraction, or the source file could not be read."
            ));
        }

        let cfg_edges = store.find_cfg_edges_by_function(sid).unwrap_or_default();

        Ok((cfg_nodes, cfg_edges))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::Store;
    use std::sync::Arc;

    fn create_test_analysis_runtime() -> AnalysisRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        AnalysisRuntime::new(store, None)
    }

    #[test]
    fn ensure_dataflow_for_unknown_symbol_returns_error() {
        let ar = create_test_analysis_runtime();
        // SymbolId::default() is an all-zero ID that won't match any symbol.
        let symbol_id = SymbolId::default();
        let result = ar.ensure_dataflow_for_function(&symbol_id, None);
        assert!(
            result.is_err(),
            "unknown SymbolId should return error, not Ok"
        );
    }
}
