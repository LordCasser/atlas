//! Graph traversal algorithms: BFS, DFS, shortest path.
//!
//! Core traversal is now implemented in `snapshot.rs`;
//! this module re-exports for backward compatibility and future extensions.

pub use super::snapshot::{TraversalConfig, TraversalDirection};
