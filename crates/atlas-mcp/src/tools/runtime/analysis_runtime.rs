//! Analysis runtime — thin ensure facade over shared Focus materialize.
//!
//! # Role
//! Semantic tools (branch_diff, lifecycle, …) need CFG/dataflow facts without
//! running the full Focus control plane (`FocusRuntime::prepare`). This type is
//! that **second door by brand only**: same [`FocusMaterialize`] stack as
//! FocusRuntime / Engine, never a second configuration.
//!
//! # Responsibilities
//! - `ensure_dataflow_for_function` / `ensure_cfg_for_function`
//! - Shares project [`FocusMaterialize`] (construct only via `from_materialize`)

use atlas_engine::{
    CfgEdge, CfgNode, FocusMaterialize, LazyDataflowService, LazyWindow, Store, SymbolId,
};

/// Thin ensure facade for CFG/dataflow over the project Focus materialize stack.
///
/// Not a second materialize configuration. Prefer this over calling dataflow
/// ensure APIs ad hoc from MCP handlers.
pub struct AnalysisRuntime {
    materialize: FocusMaterialize,
}

impl AnalysisRuntime {
    /// Build from the project-wide Focus materialize stack.
    pub fn from_materialize(materialize: FocusMaterialize) -> Self {
        Self { materialize }
    }

    /// On-demand dataflow ensure service (`LazyDataflowService` mechanism type).
    ///
    /// Name is `dataflow` (same as [`FocusMaterialize::dataflow`]), not a product path.
    #[allow(dead_code)] // shared-stack wiring tests and diagnostics
    pub fn dataflow(&self) -> &LazyDataflowService {
        self.materialize.dataflow()
    }

    /// Shared Focus materialize stack.
    #[allow(dead_code)]
    pub fn materialize(&self) -> &FocusMaterialize {
        &self.materialize
    }

    /// Trigger on-demand dataflow extraction for a function symbol.
    pub fn ensure_dataflow_for_function(
        &self,
        symbol_id: &SymbolId,
        query_id: Option<&str>,
    ) -> anyhow::Result<LazyWindow> {
        self.materialize
            .dataflow()
            .ensure_for_function(symbol_id, query_id)
    }

    /// Ensure CFG nodes and edges are available for a function, with Focus materialize fallback.
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
            self.ensure_dataflow_for_function(sid, Some(query_id))
                .map_err(|e| format!("CFG not available for analysis of '{fn_name}': {e:#}"))?;
            cfg_nodes = store
                .find_cfg_nodes_by_function(sid)
                .map_err(|e| format!("Failed to load CFG nodes after Focus materialize: {e}"))?;
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

    #[test]
    fn analysis_runtime_uses_materialize_with_rebuilder() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store = Arc::new(store);
        let m = FocusMaterialize::open(store, None);
        assert!(m.has_structural_rebuilder());
        let ar = AnalysisRuntime::from_materialize(m);
        assert!(ar.dataflow().has_structural_rebuilder());
    }
}
