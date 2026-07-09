//! Focus materialize — on-demand dataflow: planner, loader, budget constants.
//!
//! **Not a product package.** Owned by the Focus query-time solution via
//! `atlas_engine::FocusMaterialize`. Consumers use [`LazyDataflowService`]
//! only through the engine facade / Focus materialize stack.
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
use types::structs::{FactCoverage, dataflow_precision};

/// Rebuild callback type: takes a FileId, returns Ok(()) on success.
/// Injected by Focus materialize for transparent structural self-healing.
pub type StructuralRebuilder = Arc<dyn Fn(FileId) -> Result<(), anyhow::Error> + Send + Sync>;

/// On-demand dataflow ensure service (Focus materialize mechanism type).
///
/// **Do not construct ad hoc.** Obtain via [`FocusMaterialize::open`]
/// (in `atlas-engine`). The only public constructor requires a structural
/// rebuilder so unconfigured services are unrepresentable.
///
/// Wraps planner + loader behind `ensure_for_position` / `ensure_for_function`.
#[derive(Clone)]
pub struct LazyDataflowService {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    /// Structural self-heal callback (always set on public construction).
    structural_rebuilder: StructuralRebuilder,
}

impl LazyDataflowService {
    /// Create a service with a required structural self-heal rebuilder.
    ///
    /// **Not a product entry point.** Callers should use
    /// `atlas_engine::FocusMaterialize::open`, which wires the standard rebuild
    /// path. This constructor exists only so the Focus materialize factory can
    /// build a fully configured service (rebuilder is mandatory).
    #[doc(hidden)]
    pub fn with_structural_rebuilder(
        store: Arc<Store>,
        project_root: Option<PathBuf>,
        structural_rebuilder: StructuralRebuilder,
    ) -> Self {
        Self {
            store,
            project_root,
            structural_rebuilder,
        }
    }

    /// No-op rebuilder for unit tests that never hit stale-structural self-heal.
    #[cfg(test)]
    pub fn for_test(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        Self::with_structural_rebuilder(
            store,
            project_root,
            Arc::new(|_file_id| Ok(())),
        )
    }

    /// Whether a structural self-heal rebuilder is configured (always true for public construction).
    pub fn has_structural_rebuilder(&self) -> bool {
        true
    }

    /// Project root used for source path resolution (tests / diagnostics).
    pub fn project_root(&self) -> Option<&std::path::Path> {
        self.project_root.as_deref()
    }

    /// Underlying store (tests / wiring audits).
    pub fn store(&self) -> &Arc<Store> {
        &self.store
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
                    tracing::info!(
                        file = %stale_err.file_path,
                        "structural index stale; attempting self-heal rebuild"
                    );
                    match (self.structural_rebuilder)(stale_err.file_id) {
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
            window.quality = Some(precision);
        }

        // Compute capability mask from ensure result.
        // If any dataflow was produced (built or cached), set the base
        // dataflow-implying bits.
        //
        // CFG is included when at least one unit produced CFG data —
        // the loader tracks this per-unit via the language capability
        // profile and per-function CFG node counts.
        if result.units_built > 0 || result.units_cached > 0 {
            let mut mask_bits = FactCoverage::MANIFEST
                | FactCoverage::STRUCTURAL
                | FactCoverage::CALL_EDGES
                | FactCoverage::DATAFLOW;
            if result.has_cfg {
                mask_bits |= FactCoverage::CFG;
            }
            window.capability_mask = FactCoverage::from_bits(mask_bits);
        }

        Ok(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_structural_rebuilder_always_configured() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let service = LazyDataflowService::with_structural_rebuilder(
            Arc::new(Store::open_in_memory().unwrap()),
            None,
            Arc::new(move |_file_id| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }),
        );
        assert!(service.has_structural_rebuilder());
        (service.structural_rebuilder)(FileId::default()).unwrap();
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn for_test_uses_noop_rebuilder() {
        let service =
            LazyDataflowService::for_test(Arc::new(Store::open_in_memory().unwrap()), None);
        assert!(service.has_structural_rebuilder());
        (service.structural_rebuilder)(FileId::default()).unwrap();
    }
}
