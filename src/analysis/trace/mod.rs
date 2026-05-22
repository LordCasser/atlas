//! Location-driven trace queries for Atlas.
//!
//! # Architecture
//!
//! 1. **Locator** — maps a source position `(file_id, line, column)` to a
//!    [`crate::types::trace::TracePoint`] containing the reference, symbol,
//!    data node, binding, scope, and incident dataflow edges.
//! 2. **Slicer** — walks backward through dataflow edges (`Assign`, `Read`,
//!    `Write`, `FieldLoad`, `ArgToParam`, use-def) to produce a
//!    [`crate::types::trace::TracePath`] from origin to target.
//! 3. **CallerPathExplorer** — walks backward through call edges (`Calls`,
//!    `Instantiates`, `Implements`) to produce a
//!    [`crate::types::caller_path::CallerChain`] from an entry-point to a
//!    target function.
//!
//! # Relationship with other modules
//!
//! - **Locator** queries `Store` for symbols, references, scopes, data nodes,
//!   bindings, and callsites.
//! - **Slicer** queries `Store::find_dataflow_edges_by_source()` and
//!   `Store::find_dataflow_edges_by_target()` to walk the dataflow graph.
//! - **CallerPathExplorer** queries `Store::find_edges_by_target()` and
//!   `Store::find_symbol_by_id()` to walk the call graph.
//! - **Capability** determines whether dataflow tracing is available for a
//!   given language, or only symbolic lookup.

mod caller_path;
mod engine;
mod locator;
mod slicer;
pub mod virtual_edges;

pub use caller_path::CallerPathExplorer;
pub use engine::{TraceEngine, TraceQueryResponse};
pub use locator::Locator;
pub use slicer::Slicer;
