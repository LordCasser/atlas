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
//!
//! # Relationship with other modules
//!
//! - **Locator** queries `Store` for symbols, references, scopes, data nodes,
//!   bindings, and callsites.
//! - **Slicer** queries `Store::find_dataflow_edges_by_source()` and
//!   `Store::find_dataflow_edges_by_target()` to walk the dataflow graph.
//! - **Capability** determines whether dataflow tracing is available for a
//!   given language, or only symbolic lookup.

mod locator;
mod slicer;

pub use locator::Locator;
pub use slicer::Slicer;
