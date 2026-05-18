//! Atlas — Local-first semantic knowledge graph builder for codebases.
//!
//! Architecture: CLI > MCP > Context/Graph/Search/Sync > Resolution > Extraction > Database > Types

pub mod types;
pub mod db;
pub mod extraction;
pub mod resolution;
pub mod graph;
pub mod context;
pub mod search;
pub mod sync;
pub mod mcp;
#[cfg(feature = "cli")]
pub mod cli;
