//! Resolution layer: import resolver, name matcher, framework resolvers.

pub mod import_resolver;
pub mod name_matcher;
pub mod builtins;
pub mod frameworks;

use crate::db::Store;
use std::sync::Arc;

/// Three-stage reference resolution orchestrator.
pub struct ReferenceResolver {
    store: Arc<Store>,
}

impl ReferenceResolver {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    pub fn resolve_all(&self, _batch_size: usize) -> anyhow::Result<ResolutionStats> {
        todo!("M5: implement three-stage resolution pipeline")
    }
}

/// Statistics from resolution.
#[derive(Debug, Clone, Default)]
pub struct ResolutionStats {
    pub total_refs: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub by_strategy: std::collections::HashMap<String, usize>,
    pub edges_promoted: usize,
}
