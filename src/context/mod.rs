//! Context building for AI: hybrid search, formatting.

pub mod search;
pub mod formatter;

use crate::db::Store;
use crate::graph::GraphEngine;
use std::sync::Arc;

/// AI context builder.
pub struct ContextBuilder {
    store: Arc<Store>,
    graph: Arc<GraphEngine>,
}

impl ContextBuilder {
    pub fn new(store: Arc<Store>, graph: Arc<GraphEngine>) -> Self {
        Self { store, graph }
    }
}
