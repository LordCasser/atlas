//! Atlas — Local-first semantic knowledge graph builder for codebases.
//!
//! Architecture: CLI > MCP > Context/Graph/Search/Sync > Analysis > Resolution > Extraction > Database > Types

pub mod analysis;
#[cfg(feature = "cli")]
pub mod cli;
pub mod context;
pub mod db;
pub mod extraction;
pub mod graph;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod resolution;
pub mod search;
pub mod sync;
pub mod types;
