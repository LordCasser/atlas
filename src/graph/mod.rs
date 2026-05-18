//! Graph layer: traversal, call graphs, impact analysis.

pub mod traversal;
pub mod queries;

use crate::db::Store;
use std::sync::Arc;

/// High-level graph query engine.
pub struct GraphEngine {
    store: Arc<Store>,
}

impl GraphEngine {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}
