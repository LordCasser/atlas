//! Analysis layer: location-driven trace queries, call-graph exploration,
//! and lightweight function summaries.
//!
//! Architecture:
//! - `trace/` — variable tracking, caller-path, and trace-point resolution
//!   (production API exposed via CLI and MCP)
//! - `summary` — query-time FunctionSummary builder for intraprocedural
//!   reachability (parameter → return / call-arg / field)
//! - `cross_function` — inter-procedural bridging via persisted summaries
//!   (CrossFunctionBridge) with runtime fallback

pub mod cross_function;
pub mod summary;
pub mod trace;
