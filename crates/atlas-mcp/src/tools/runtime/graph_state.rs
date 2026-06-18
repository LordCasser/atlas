//! Graph state management: lifecycle of in-memory graph snapshots.
//!
//! Uses interior mutability (`Mutex`, `AtomicBool`) so that methods take
//! `&self`, enabling `Arc<ActiveProject>` to share state without external locks.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use atlas_engine::{
    ContextBuilder, ContextView, FileId, GraphEngine, SearchEngine, SourceExtractor, Store,
};

use crate::tools::lazy_refresh;
use crate::tools::runtime::graph_provider::GraphProvider;

pub(crate) struct GraphState {
    pub(crate) search: Mutex<Option<SearchEngine>>,
    pub(crate) context: Mutex<Option<ContextBuilder>>,
    pub(crate) graph_initialized: AtomicBool,
    pub(crate) last_graph_signature: Mutex<String>,
    pub(crate) pending_graph_rebuild: Arc<Mutex<Option<Arc<GraphEngine>>>>,
}

impl GraphState {
    pub(crate) fn init_with(&self, search: SearchEngine, context: ContextBuilder) {
        *self.search.lock().unwrap() = Some(search);
        *self.context.lock().unwrap() = Some(context);
        self.graph_initialized.store(true, Ordering::Release);
    }

    pub(crate) fn ensure_initialized(
        &self,
        store: &Arc<Store>,
        source_extractor: &SourceExtractor,
        project_root: &Path,
    ) -> anyhow::Result<()> {
        if self.graph_initialized.load(Ordering::Acquire) {
            return Ok(());
        }
        tracing::info!("Building graph snapshot (first request)...");
        let graph = Arc::new(GraphEngine::from_store(store, 0.3)?);
        *self.search.lock().unwrap() =
            Some(SearchEngine::new(Arc::clone(store), Arc::clone(&graph)));
        let ctx = ContextBuilder::new(Arc::clone(store), graph)
            .with_project_root(project_root.to_path_buf());
        let ext = source_extractor.clone();
        *self.context.lock().unwrap() = Some(ctx.with_source_fn(Arc::new(ext)));
        *self.last_graph_signature.lock().unwrap() = store.index_signature().unwrap_or_default();
        self.graph_initialized.store(true, Ordering::Release);
        tracing::info!("Graph snapshot ready.");
        Ok(())
    }

    pub(crate) fn edge_count(&self) -> usize {
        self.search
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.graph_snapshot().edge_count()))
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn symbol_count(&self) -> usize {
        self.search
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.graph_snapshot().node_count()))
            .unwrap_or(0)
    }

    pub(crate) fn swap_graph(&self, store: &Store, graph: Arc<GraphEngine>) {
        if let Ok(mut g) = self.search.lock() {
            if let Some(ref mut s) = *g {
                s.refresh_graph(Arc::clone(&graph));
            }
        }
        if let Ok(mut g) = self.context.lock() {
            if let Some(ref mut c) = *g {
                c.refresh_graph(graph);
            }
        }
        *self.last_graph_signature.lock().unwrap() = store.index_signature().unwrap_or_default();
    }

    pub(crate) fn try_apply_or_spawn_rebuild(
        &self,
        store: Arc<Store>,
        lazy_refresh_queue: Arc<lazy_refresh::LazyRefreshQueue>,
    ) {
        if let Some(graph) = self
            .pending_graph_rebuild
            .lock()
            .ok()
            .and_then(|mut p| p.take())
        {
            tracing::info!("Applying background-built graph snapshot");
            self.swap_graph(&store, graph);
            lazy_refresh_queue.mark_rebuild_applied();
            lazy_refresh_queue.mark_rebuild_finished();
            return;
        }

        if lazy_refresh_queue.needs_full_rebuild() && lazy_refresh_queue.try_start_rebuild() {
            tracing::info!("Spawning background full graph rebuild (non-blocking)");
            let pending = Arc::clone(&self.pending_graph_rebuild);
            let queue = Arc::clone(&lazy_refresh_queue);
            std::thread::spawn(move || match GraphEngine::from_store(&store, 0.3) {
                Ok(graph) => {
                    if let Ok(mut slot) = pending.lock() {
                        *slot = Some(Arc::new(graph));
                    }
                }
                Err(e) => {
                    tracing::error!("Background graph rebuild failed: {:#}", e);
                    queue.mark_rebuild_finished();
                    queue.schedule_full_rebuild();
                }
            });
        }
    }

    pub(crate) fn refresh_graph_for_files(
        &self,
        store: &Store,
        file_ids: &[FileId],
    ) -> anyhow::Result<()> {
        if !self.graph_initialized.load(Ordering::Acquire) || file_ids.is_empty() {
            return Ok(());
        }

        const REPLACE_THRESHOLD: usize = 500;
        if file_ids.len() > REPLACE_THRESHOLD {
            let current = store.index_signature().unwrap_or_default();
            if current != *self.last_graph_signature.lock().unwrap() {
                tracing::info!("Index signature changed, refreshing graph (large batch fallback)");
                let graph = Arc::new(GraphEngine::from_store(store, 0.3)?);
                self.swap_graph(store, graph);
            }
            return Ok(());
        }

        let old_graph = match self.search.lock().ok() {
            Some(g) if g.is_some() => g.as_ref().unwrap().graph_snapshot(),
            _ => {
                let current = store.index_signature().unwrap_or_default();
                if current != *self.last_graph_signature.lock().unwrap() {
                    tracing::info!(
                        "Index signature changed, refreshing graph (no existing snapshot)"
                    );
                    let graph = Arc::new(GraphEngine::from_store(store, 0.3)?);
                    self.swap_graph(store, graph);
                }
                return Ok(());
            }
        };
        let file_paths: std::collections::HashMap<FileId, String> = store
            .list_files()
            .unwrap_or_default()
            .into_iter()
            .map(|f| (f.file_id, f.path))
            .collect();

        let old_snap = old_graph.snapshot();
        let mut new_snapshot = old_snap.clone();
        new_snapshot.replace_files_in_place(store, file_ids, 0.3, &file_paths)?;

        let new_graph = Arc::new(GraphEngine::from_snapshot(new_snapshot));
        if let Ok(mut g) = self.search.lock() {
            if let Some(ref mut s) = *g {
                s.refresh_graph(Arc::clone(&new_graph));
            }
        }
        if let Ok(mut g) = self.context.lock() {
            if let Some(ref mut c) = *g {
                c.refresh_graph(new_graph);
            }
        }
        *self.last_graph_signature.lock().unwrap() = store.index_signature().unwrap_or_default();
        Ok(())
    }

    /// Lock and access the search engine for a closure.
    fn with_search<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&SearchEngine) -> R,
    {
        let guard = self.search.lock().ok()?;
        guard.as_ref().map(|s| f(s))
    }

    /// Lock and access the context builder for a closure.
    fn with_context<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&ContextBuilder) -> R,
    {
        let guard = self.context.lock().ok()?;
        guard.as_ref().map(|c| f(c))
    }
}

impl Clone for GraphState {
    fn clone(&self) -> Self {
        Self {
            search: Mutex::new(None),
            context: Mutex::new(None),
            graph_initialized: AtomicBool::new(self.graph_initialized.load(Ordering::Acquire)),
            last_graph_signature: Mutex::new(self.last_graph_signature.lock().unwrap().clone()),
            pending_graph_rebuild: Arc::clone(&self.pending_graph_rebuild),
        }
    }
}

impl GraphProvider for GraphState {
    fn is_initialized(&self) -> bool {
        self.graph_initialized.load(Ordering::Acquire)
    }

    fn graph_snapshot(&self) -> Option<Arc<GraphEngine>> {
        self.with_search(|s| s.graph_snapshot())
    }

    fn build_context_for_symbol(
        &self,
        sid: &atlas_engine::SymbolId,
        include_file_peers: bool,
    ) -> Option<Result<ContextView, anyhow::Error>> {
        self.with_context(|c| c.build_context_for_symbol(sid, include_file_peers))
    }
}
