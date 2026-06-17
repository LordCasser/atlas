//! Lazy dataflow engine: planner, loader, and hardcoded budget constants.
//!
//! This crate is NOT a public API — consumers use [`LazyDataflowService`]
//! through the `atlas-engine` facade.
//!
//! # Crate boundaries
//! - `planner`: reads structural index from `db`, produces [`LazyWindow`]
//! - `loader`: reads/writes `db`, calls `extraction`, manages unit extraction state
//! - `constants`: hardcoded budget caps (never exposed to MCP/CLI)

mod constants;
mod loader;
mod planner;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use types::StaleStructuralIndexError;
use types::ids::{FileId, SymbolId};
use types::lazy::LazyWindow;
use types::structs::{CapabilityMask, dataflow_precision};

/// Rebuild callback type: takes a FileId, returns Ok(()) on success.
/// Injected by the engine layer to enable transparent self-healing.
type StructuralRebuilder = Arc<dyn Fn(FileId) -> Result<(), anyhow::Error> + Send + Sync>;

/// Public entry point for the `atlas-engine` facade.
///
/// Wraps planner + loader behind a single `ensure_for_position` /
/// `ensure_for_function` API.  The facade calls this before delegating
/// to `analysis::TraceEngine`.
#[derive(Clone)]
pub struct LazyDataflowService {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    /// Optional callback for rebuilding a stale structural index.
    /// Set by the engine layer; the lazy crate calls it before retrying.
    structural_rebuilder: Option<StructuralRebuilder>,
}

impl LazyDataflowService {
    /// Create a new service backed by the given store.
    ///
    /// When `project_root` is provided, the loader resolves relative file
    /// paths against it when reading source files for lazy extraction.
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        Self {
            store,
            project_root,
            structural_rebuilder: None, // Set separately by engine layer
        }
    }

    /// Set the structural rebuild callback.
    /// Called once during engine construction.
    pub fn set_structural_rebuilder(&mut self, rebuilder: StructuralRebuilder) {
        self.structural_rebuilder = Some(rebuilder);
    }

    /// Plan a window and ensure all units have dataflow built.
    ///
    /// Returns the window (including a `truncated` flag if the budget
    /// was exceeded).  The caller should merge any `truncated` diagnostic
    /// into the final trace response.
    pub fn ensure_for_position(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
        trigger_query: Option<&str>,
    ) -> Result<LazyWindow> {
        let window =
            planner::LazyDataflowPlanner::plan_for_position(&self.store, file_id, line, column)?;
        self.ensure_window(window, trigger_query)
    }

    /// Plan a window for a known symbol and ensure all units have dataflow.
    pub fn ensure_for_function(
        &self,
        symbol_id: &SymbolId,
        trigger_query: Option<&str>,
    ) -> Result<LazyWindow> {
        let window = planner::LazyDataflowPlanner::plan_for_function(&self.store, symbol_id)?;
        self.ensure_window(window, trigger_query)
    }

    /// Shared post-plan logic: load, compute precision tier & capability mask.
    fn ensure_window(
        &self,
        mut window: LazyWindow,
        trigger_query: Option<&str>,
    ) -> Result<LazyWindow> {
        let result = loader::LazyDataflowLoader::ensure(
            &self.store,
            &window,
            self.project_root.as_deref(),
            trigger_query,
        );

        // Handle stale structural index with self-healing retry
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                // Check if it's a StaleStructuralIndexError we can self-heal
                if let Some(stale_err) = e.downcast_ref::<StaleStructuralIndexError>() {
                    if let Some(rebuilder) = &self.structural_rebuilder {
                        tracing::info!(
                            file = %stale_err.file_path,
                            "structural index stale; attempting self-heal rebuild"
                        );
                        match rebuilder(stale_err.file_id) {
                            Ok(()) => {
                                tracing::info!(
                                    file = %stale_err.file_path,
                                    "self-heal rebuild succeeded; retrying dataflow"
                                );
                                // Retry exactly ONCE
                                loader::LazyDataflowLoader::ensure(
                                    &self.store,
                                    &window,
                                    self.project_root.as_deref(),
                                    trigger_query,
                                )?
                            }
                            Err(rebuild_err) => {
                                tracing::warn!(
                                    file = %stale_err.file_path,
                                    "self-heal rebuild failed: {rebuild_err:#}"
                                );
                                return Err(anyhow::anyhow!(
                                    "self-heal rebuild failed for {}: {rebuild_err:#}",
                                    stale_err.file_path,
                                ));
                            }
                        }
                    } else {
                        // No rebuilder configured — propagate original error
                        return Err(e);
                    }
                } else {
                    // Not a stale index error — propagate as-is
                    return Err(e);
                }
            }
        };

        window.truncated = window.truncated || result.budget_exceeded;
        window.units_built = result.units_built;
        window.units_cached = result.units_cached;
        window.units_pending = result.units_pending;
        window.pending_job_ids = result.pending_job_ids;

        // Compute dataflow precision
        {
            let planned = window.units.len();
            let available = result.units_built + result.units_cached;
            let incomplete = result.budget_exceeded || result.units_pending > 0;
            let precision = dataflow_precision(available, planned, incomplete);
            window.precision = Some(precision);
        }

        // Compute capability mask from ensure result.
        // If any dataflow was produced (built or cached), set the base
        // dataflow-implying bits.
        //
        // CFG is included when at least one unit produced CFG data —
        // the loader tracks this per-unit via the language capability
        // profile and per-function CFG node counts.
        if result.units_built > 0 || result.units_cached > 0 {
            let mut mask_bits = CapabilityMask::MANIFEST
                | CapabilityMask::STRUCTURAL
                | CapabilityMask::CALL_EDGES
                | CapabilityMask::DATAFLOW;
            if result.has_cfg {
                mask_bits |= CapabilityMask::CFG;
            }
            window.capability_mask = CapabilityMask::from_bits(mask_bits);
        }

        Ok(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_constructed_without_rebuilder() {
        let service = LazyDataflowService::new(Arc::new(Store::open_in_memory().unwrap()), None);
        assert!(service.structural_rebuilder.is_none());
    }

    #[test]
    fn service_set_rebuilder() {
        let mut service =
            LazyDataflowService::new(Arc::new(Store::open_in_memory().unwrap()), None);
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        service.set_structural_rebuilder(Arc::new(move |_file_id| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        assert!(service.structural_rebuilder.is_some());
        // Call the callback
        let rebuilder = service.structural_rebuilder.as_ref().unwrap();
        rebuilder(FileId::default()).unwrap();
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
