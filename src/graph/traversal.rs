//! Graph traversal algorithms: BFS, DFS, shortest path.

/// Direction of graph traversal.
#[derive(Debug, Clone, Copy)]
pub enum TraversalDirection {
    Incoming,
    Outgoing,
    Both,
}

/// Configuration for graph traversal.
#[derive(Debug, Clone)]
pub struct TraversalConfig {
    pub direction: TraversalDirection,
    pub max_depth: usize,
    pub limit: usize,
}

impl Default for TraversalConfig {
    fn default() -> Self {
        Self {
            direction: TraversalDirection::Outgoing,
            max_depth: 3,
            limit: 100,
        }
    }
}

// TODO: Phase 9 — implement BFS, DFS, shortest path
