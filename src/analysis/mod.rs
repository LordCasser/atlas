//! Analysis layer: location-driven trace queries and call-graph exploration.
//!
//! Architecture:
//! - `trace/` — variable tracking, caller-path, and trace-point resolution
//!   (production API exposed via CLI and MCP)

pub mod trace;
