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
pub(crate) const LAZY_DATAFLOW_MAX_UNITS: usize = 100;

/// Wall-clock time budget for a single lazy-load operation (milliseconds).
pub(crate) const LAZY_DATAFLOW_BUDGET_MS: u64 = 20_000;

/// Layer identifier for dataflow extraction state.
pub(crate) const LAYER_DATAFLOW: &str = "dataflow";

/// Status for a unit whose extraction completed within budget.
pub(crate) const STATUS_COMPLETE: &str = "complete";

/// Status for a unit whose extraction exceeded budget (partial result).
pub(crate) const STATUS_PARTIAL: &str = "partial";

// ── Layering (do not merge casually) ──────────────────────────────────────
// - FocusWindowBudget (atlas-engine focus/types): foreground 18s / bg 60s wall
//   clocks for structural expansion loops.
// - LAZY_DATAFLOW_BUDGET_MS (here): unit dataflow ensure wall clock (20s).
// - LAZY_MAX_NODES/EDGES_PER_UNIT: extraction/src/mode.rs (extraction cannot
//   depend on this crate). Adjust both docs when changing caps.
