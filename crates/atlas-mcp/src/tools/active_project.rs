use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use atlas_engine::Engine;
use atlas_engine::Store;

use crate::tools::lazy_refresh::LazyRefreshQueue;

use super::runtime::{
    analysis_runtime::AnalysisRuntime, graph_runtime::GraphRuntime,
    invalidation::RuntimeInvalidation, job_runtime::JobRuntime, overlay_runtime::OverlayRuntime,
    query_runtime::QueryRuntime, store_query_runtime::StoreQueryRuntime,
};

/// The active project aggregate.
///
/// Replaces the v5.0 ToolRouter God-object. Each runtime owns a
/// clearly scoped responsibility. Constructed once during
/// `open_project` activation.
pub struct ActiveProject {
    pub root: PathBuf,
    pub store: Arc<Store>,
    /// High-level Engine wrapping the full extraction → trace pipeline.
    /// Wrapped in Mutex because Engine contains RefCell (Send but not Sync).
    pub engine: Mutex<Engine>,

    pub query_runtime: QueryRuntime,
    pub graph_runtime: GraphRuntime,
    pub analysis_runtime: AnalysisRuntime,
    pub overlay_runtime: OverlayRuntime,
    pub store_query_runtime: StoreQueryRuntime,
    pub job_runtime: JobRuntime,
}

impl ActiveProject {
    /// Create a new ActiveProject from a store and project root.
    /// This is the single construction point — replaces ToolRouter::new()
    /// and ToolRouter::new_empty().
    pub fn new(store: Arc<Store>, root: PathBuf) -> Result<Arc<Self>> {
        let lazy_refresh_queue = LazyRefreshQueue::new();
        let invalidation = Arc::new(RuntimeInvalidation::new());

        let query_runtime = QueryRuntime::new(
            store.clone(),
            Some(root.clone()),
            lazy_refresh_queue.clone(),
        );

        let source_extractor = atlas_engine::SourceExtractor::new(store.clone(), root.clone());

        let graph_runtime = GraphRuntime::new(
            store.clone(),
            source_extractor.clone(),
            root.clone(),
            invalidation.clone(),
        );

        let store_query_runtime = StoreQueryRuntime::new(store.clone(), root.clone());

        let engine = Engine::from_store(store.clone(), Some(root.as_ref()));

        Ok(Arc::new(Self {
            query_runtime,
            graph_runtime,
            analysis_runtime: AnalysisRuntime::new(store.clone(), Some(root.clone())),
            overlay_runtime: OverlayRuntime::new(store.clone(), invalidation),
            store_query_runtime,
            job_runtime: JobRuntime::new(),
            engine: Mutex::new(engine),
            store,
            root,
        }))
    }
}
