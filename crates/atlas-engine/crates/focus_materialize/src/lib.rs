//! Focus materialize — on-demand dataflow: planner, loader, budget constants.
//!
//! **Not a product package.** Owned by the Focus query-time solution via
//! `atlas_engine::FocusMaterialize`. Consumers use [`LazyDataflowService`]
//! only through the engine facade / Focus materialize stack.
//!
//! # Naming
//! Types keep the `Lazy*` prefix as a **mechanism** name (CS deferred evaluation:
//! build unit dataflow only when a query needs it). That is **not** an
//! `AccessStrategy` or product path — product paths are Index / Focus only.
//!
//! # Construction contract
//! Do **not** construct [`LazyDataflowService`] outside
//! `atlas_engine::FocusMaterialize::open` (or test helpers). The public factory
//! is `#[doc(hidden)]` so cross-crate wiring can inject the structural rebuilder;
//! ad-hoc rebuilders are unsupported.
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
    /// **Factory API only (N1).** Cross-crate visibility is required so
    /// `atlas_engine::FocusMaterialize::open` can inject the standard rebuild
    /// path. Unconfigured services are unrepresentable; non-standard rebuilders
    /// are unsupported outside that factory / unit tests.
    ///
    /// Prefer `FocusMaterialize::open` or [`Self::for_test`].
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
        Self::with_structural_rebuilder(store, project_root, Arc::new(|_file_id| Ok(())))
    }

    /// Always `true` after public construction: the rebuilder is a required field.
    ///
    /// Kept as an explicit audit/wiring probe (MCP/tests assert configuration
    /// identity). Prefer checking that ensure/self-heal paths run over treating
    /// this as a runtime branch.
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
        self.ensure_for_position_with_depth(
            file_id,
            line,
            column,
            crate::constants::LAZY_DATAFLOW_MAX_DEPTH,
            trigger_query,
        )
    }

    /// Plan and ensure a position-centered window at a caller-provided Focus depth.
    pub fn ensure_for_position_with_depth(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
        max_depth: usize,
        trigger_query: Option<&str>,
    ) -> Result<LazyWindow> {
        let window = planner::LazyDataflowPlanner::plan_for_position_with_depth(
            &self.store,
            file_id,
            line,
            column,
            max_depth,
        )?;
        self.ensure_window(window, trigger_query)
    }

    /// Plan a window for a known symbol and ensure all units have dataflow.
    pub fn ensure_for_function(
        &self,
        symbol_id: &SymbolId,
        trigger_query: Option<&str>,
    ) -> Result<LazyWindow> {
        self.ensure_for_function_with_depth(
            symbol_id,
            crate::constants::LAZY_DATAFLOW_MAX_DEPTH,
            trigger_query,
        )
    }

    /// Plan and ensure a callable-centered window at a caller-provided Focus depth.
    pub fn ensure_for_function_with_depth(
        &self,
        symbol_id: &SymbolId,
        max_depth: usize,
        trigger_query: Option<&str>,
    ) -> Result<LazyWindow> {
        let window = planner::LazyDataflowPlanner::plan_for_function_with_depth(
            &self.store,
            symbol_id,
            max_depth,
        )?;
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

        // Window-level summary mask (not per-unit truth). Base dataflow bits
        // if any unit was built/cached; CFG when any unit produced CFG.
        // CALL_EDGES is OR of per-unit gates (structural freshness + callsites).
        if result.units_built > 0 || result.units_cached > 0 {
            let mut mask_bits =
                FactCoverage::MANIFEST | FactCoverage::STRUCTURAL | FactCoverage::DATAFLOW;
            if window
                .units
                .iter()
                .any(|u| loader::unit_has_call_edges(&self.store, u))
            {
                mask_bits |= FactCoverage::CALL_EDGES;
            }
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
    fn with_structural_rebuilder_invokes_injected_callback() {
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
        (service.structural_rebuilder)(FileId::default()).unwrap();
        assert!(
            called.load(std::sync::atomic::Ordering::SeqCst),
            "injected structural rebuilder must run on invoke"
        );
    }

    #[test]
    fn for_test_rebuilder_is_callable_noop() {
        let service =
            LazyDataflowService::for_test(Arc::new(Store::open_in_memory().unwrap()), None);
        // No-op rebuilder must not error (self-heal path stays usable in unit tests).
        (service.structural_rebuilder)(FileId::default()).unwrap();
    }
}
