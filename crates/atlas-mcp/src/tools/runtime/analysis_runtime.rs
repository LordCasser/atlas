//! Analysis runtime — on-demand CFG/dataflow extraction.
//!
//! # Responsibilities
//! - Single entry point (`ensure_dataflow_for_function`) for lazy CFG extraction
//! - Wraps LazyDataflowService — no other module calls it directly
//!
//! # Usage pattern
//! ```ignore
//! self.active.analysis_runtime.ensure_dataflow_for_function(&symbol_id, Some(&query_id))?;
//! ```
//!
//! # Dependencies
//! - `atlas_engine::LazyDataflowService`

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::{LazyDataflowService, Store, SymbolId};

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
        Self {
            lazy_service,
        }
    }

    /// Trigger lazy dataflow extraction for a function symbol.
    ///
    /// If CFG nodes already exist for this function, returns `Ok(())` immediately.
    /// Otherwise, triggers on-demand CFG + dataflow build.
    pub fn ensure_dataflow_for_function(
        &self,
        symbol_id: &SymbolId,
        query_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let _ = self.lazy_service.ensure_for_function(symbol_id, query_id)?;
        Ok(())
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
