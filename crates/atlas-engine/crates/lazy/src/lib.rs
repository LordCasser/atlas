//! Lazy dataflow engine: planner, loader, and hardcoded budget constants.
//!
//! This crate is NOT a public API — consumers use [`LazyDataflowService`]
//! through the `atlas-engine` facade.
//!
//! # Crate boundaries
//! - `planner`: reads structural index from `db`, produces [`LazyWindow`]
//! - `loader`: reads/writes `db`, calls `extraction`, manages artifacts
//! - `constants`: hardcoded budget caps (never exposed to MCP/CLI)

mod constants;
mod loader;
mod planner;

use std::sync::Arc;

use anyhow::Result;
use db::Store;
use types::ids::{FileId, SymbolId};
use types::lazy::LazyWindow;

/// Public entry point for the `atlas-engine` facade.
///
/// Wraps planner + loader behind a single `ensure_for_position` /
/// `ensure_for_function` API.  The facade calls this before delegating
/// to `analysis::TraceEngine`.
pub struct LazyDataflowService {
    store: Arc<Store>,
}

impl LazyDataflowService {
    /// Create a new service backed by the given store.
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
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
    ) -> Result<LazyWindow> {
        let mut window =
            planner::LazyDataflowPlanner::plan_for_position(&self.store, file_id, line, column)?;
        let result = loader::LazyDataflowLoader::ensure(&self.store, &window)?;
        window.truncated = window.truncated || result.budget_exceeded;
        Ok(window)
    }

    /// Plan a window for a known symbol and ensure all units have dataflow.
    pub fn ensure_for_function(&self, symbol_id: &SymbolId) -> Result<LazyWindow> {
        let mut window =
            planner::LazyDataflowPlanner::plan_for_function(&self.store, symbol_id)?;
        let result = loader::LazyDataflowLoader::ensure(&self.store, &window)?;
        window.truncated = window.truncated || result.budget_exceeded;
        Ok(window)
    }
}
