//! Sync engine: incremental update, file watching.

pub mod detector;

#[cfg(feature = "sync")]
pub mod watcher;

use crate::db::Store;
use std::sync::Arc;

/// Incremental sync engine.
pub struct SyncEngine {
    store: Arc<Store>,
}

impl SyncEngine {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

/// Statistics from a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub files_changed: usize,
    pub files_reindexed: usize,
    pub files_removed: usize,
    pub new_nodes: usize,
    pub new_edges: usize,
    pub duration: std::time::Duration,
}
