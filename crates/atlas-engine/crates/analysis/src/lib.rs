//! Analysis layer: location-driven trace queries, call-graph exploration,
//! and lightweight function summaries.
//!
//! Architecture:
//! - `trace/` — variable tracking, caller-path, and trace-point resolution
//!   (production API exposed via CLI and MCP)
//! - `summary` — query-time FunctionSummary builder for intraprocedural
//!   reachability (parameter → return / call-arg / field)

pub mod summary;
pub mod trace;
