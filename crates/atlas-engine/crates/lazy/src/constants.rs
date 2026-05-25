//! Hardcoded budget constants for lazy dataflow loading.
//!
//! These values are NOT exposed to MCP tools, CLI parameters, or any external
//! configuration.  They exist solely as internal compile-time constants that
//! may be adjusted during development/testing based on empirical performance
//! data.
//!
//! # Invariant
//! Every constant in this module is `pub(crate)` — no re-exports at the
//! crate root, no `pub use` in the facade.

/// Maximum expansion depth from the seed unit.
/// 0 = only the seed function, 1 = direct callers/callees, 2 = transitive.
pub(crate) const LAZY_DATAFLOW_MAX_DEPTH: usize = 2;

/// Hard cap on the total number of AnalysisUnits in a single LazyWindow.
pub(crate) const LAZY_DATAFLOW_MAX_UNITS: usize = 64;

/// Maximum DataNode count for a single AnalysisUnit (excess nodes dropped).
pub(crate) const LAZY_DATAFLOW_MAX_NODES_PER_UNIT: usize = 2_000;

/// Maximum DataFlowEdge count for a single AnalysisUnit.
pub(crate) const LAZY_DATAFLOW_MAX_EDGES_PER_UNIT: usize = 20_000;

/// Wall-clock time budget for a single lazy-load operation (milliseconds).
pub(crate) const LAZY_DATAFLOW_BUDGET_MS: u64 = 25_000;
