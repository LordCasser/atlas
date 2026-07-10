use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use atlas_engine::{Engine, FocusMaterialize, Store};

use crate::tools::lazy_refresh::LazyRefreshQueue;

use super::runtime::{
    analysis_runtime::AnalysisRuntime, graph_runtime::GraphRuntime,
    invalidation::RuntimeInvalidation, job_runtime::JobRuntime, overlay_runtime::OverlayRuntime,
    query_runtime::QueryRuntime, store_query_runtime::StoreQueryRuntime,
};

/// The active project aggregate.
///
/// Owns a single [`FocusMaterialize`] stack shared by FocusRuntime, Engine,
/// and AnalysisRuntime (query-time Focus path only).
pub struct ActiveProject {
    pub root: PathBuf,
    pub store: Arc<Store>,
    /// High-level Engine wrapping extraction → trace (shares Focus materialize).
    pub engine: Mutex<Engine>,
    /// Project-wide Focus materialize (structural + dataflow + rebuilder).
    pub materialize: FocusMaterialize,

    pub query_runtime: QueryRuntime,
    pub graph_runtime: GraphRuntime,
    pub analysis_runtime: AnalysisRuntime,
    pub overlay_runtime: OverlayRuntime,
    pub store_query_runtime: StoreQueryRuntime,
    pub job_runtime: JobRuntime,
}

impl ActiveProject {
    /// Create a new ActiveProject from a store and project root.
    ///
    /// Single construction point for Focus materialize configuration.
    pub fn new(store: Arc<Store>, root: PathBuf) -> Result<Arc<Self>> {
        let lazy_refresh_queue = LazyRefreshQueue::new();
        let invalidation = Arc::new(RuntimeInvalidation::new());

        // One Focus materialize stack for the project (required at FocusRuntime construction).
        let materialize = FocusMaterialize::open(store.clone(), Some(root.clone()));

        let query_runtime = QueryRuntime::new(
            store.clone(),
            Some(root.clone()),
            materialize.clone(),
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

        let engine =
            Engine::from_materialize(store.clone(), materialize.clone(), Some(root.as_ref()));

        let analysis_runtime = AnalysisRuntime::from_materialize(materialize.clone());

        Ok(Arc::new(Self {
            query_runtime,
            graph_runtime,
            analysis_runtime,
            overlay_runtime: OverlayRuntime::new(store.clone(), invalidation),
            store_query_runtime,
            job_runtime: JobRuntime::new(),
            engine: Mutex::new(engine),
            materialize,
            store,
            root,
        }))
    }
}
