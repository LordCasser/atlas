//! High-level graph queries: call graph, type hierarchy, import analysis.
//!
//! The `GraphEngine` in `mod.rs` provides the primary query API.
//! This module holds domain-specific subgraph types.

pub use super::snapshot::{CallGraphView, GraphPath, Subgraph};
