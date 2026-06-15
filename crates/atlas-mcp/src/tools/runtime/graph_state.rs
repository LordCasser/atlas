//! Graph state management: lifecycle of in-memory graph snapshots.
//!
//! Extracted from [`super::ToolRouter`] to reduce the God-object footprint.
//! Owns `SearchEngine`, `ContextBuilder`, and background-rebuild machinery.

use std::path::Path;
use std::sync::{Arc, Mutex};

use atlas_engine::{ContextBuilder, FileId, GraphEngine, SearchEngine, SourceExtractor, Store};

use crate::tools::lazy_refresh;
use crate::tools::runtime::graph_provider::GraphProvider;

/// Graph snapshot lifecycle: lazy initialization, incremental refresh,
/// and background full-rebuild coordination.
pub(crate) struct GraphState {
    pub(crate) search: Option<SearchEngine>,
    pub(crate) context: Option<ContextBuilder>,
    pub(crate) graph_initialized: bool,
    pub(crate) last_graph_signature: String,
    pub(crate) pending_graph_rebuild: Arc<Mutex<Option<Arc<GraphEngine>>>>,
}

impl GraphState {
    /// Initialize graph state with pre-built search and context engines.
    ///
    /// Used by `ToolRouter::new()` when the caller already has pre-built
    /// `SearchEngine` and `ContextBuilder` (e.g. integration tests).
    pub(crate) fn init_with(&mut self, search: SearchEngine, context: ContextBuilder) {
        self.search = Some(search);
        self.context = Some(context);
        self.graph_initialized = true;
    }

    /// Build the graph engines on first use.
    ///
    /// Called only for graph-backed tool calls after the MCP handshake
    /// completes, so the client doesn't timeout waiting for a startup response.
    pub(crate) fn ensure_initialized(
        &mut self,
        store: &Arc<Store>,
        source_extractor: &SourceExtractor,
        project_root: &Path,
    ) -> anyhow::Result<()> {
        if self.graph_initialized {
            return Ok(());
        }
        tracing::info!("Building graph snapshot (first request)...");
        let graph = Arc::new(GraphEngine::from_store(store, 0.3)?);
        self.search = Some(SearchEngine::new(Arc::clone(store), Arc::clone(&graph)));
        self.context = Some(
            ContextBuilder::new(Arc::clone(store), graph)
                .with_project_root(project_root.to_path_buf()),
        );
        // Register AST-aware source extraction reader.
        let ext = source_extractor.clone();
        if let Some(ctx) = self.context.take() {
            self.context = Some(ctx.with_source_fn(Arc::new(ext)));
        }
        self.last_graph_signature = store.index_signature().unwrap_or_default();
        self.graph_initialized = true;
        tracing::info!("Graph snapshot ready.");
        Ok(())
    }

    /// Return the number of edges in the current graph snapshot (0 if not init).
    pub(crate) fn edge_count(&self) -> usize {
        self.search
            .as_ref()
            .map(|s| s.graph_snapshot().edge_count())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn symbol_count(&self) -> usize {
        self.search
            .as_ref()
            .map(|s| s.graph_snapshot().node_count())
            .unwrap_or(0)
    }

    /// Atomically swap in a pre-built graph, updating both search and context engines.
    pub(crate) fn swap_graph(&mut self, store: &Store, graph: Arc<GraphEngine>) {
        if let Some(ref mut s) = self.search {
            s.refresh_graph(Arc::clone(&graph));
        }
        if let Some(ref mut c) = self.context {
            c.refresh_graph(graph);
        }
        self.last_graph_signature = store.index_signature().unwrap_or_default();
    }

    /// Try to apply a background-built graph from the pending slot,
    /// or spawn a background rebuild thread if one was scheduled.
    ///
    /// Step 1: If a pending graph exists (built by a previous background thread),
    /// swap it in and clear the flags.
    /// Step 2: If a full rebuild was scheduled (cumulative threshold reached),
    /// and no rebuild is in progress, spawn a background thread to build the
    /// graph from the store. The current request continues with the old snapshot.
    pub(crate) fn try_apply_or_spawn_rebuild(
        &mut self,
        store: Arc<Store>,
        lazy_refresh_queue: Arc<lazy_refresh::LazyRefreshQueue>,
    ) {
        // Step 1: Check for a pre-built graph in the pending slot.
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

        // Step 2: If a full rebuild is needed and no rebuild is in progress,
        // spawn a background thread to build the graph.
        if lazy_refresh_queue.needs_full_rebuild() && lazy_refresh_queue.try_start_rebuild() {
            tracing::info!("Spawning background full graph rebuild (non-blocking)");
            let pending = Arc::clone(&self.pending_graph_rebuild);
            let queue = Arc::clone(&lazy_refresh_queue);
            std::thread::spawn(move || {
                match GraphEngine::from_store(&store, 0.3) {
                    Ok(graph) => {
                        if let Ok(mut slot) = pending.lock() {
                            *slot = Some(Arc::new(graph));
                        }
                        // Note: rebuild_in_progress stays true until the pending
                        // graph is picked up by a subsequent try_apply_or_spawn_rebuild.
                    }
                    Err(e) => {
                        tracing::error!("Background graph rebuild failed: {:#}", e);
                        queue.mark_rebuild_finished();
                        queue.schedule_full_rebuild(); // retry on next call
                    }
                }
            });
        }
    }

    /// Refresh graph after lazy structural extraction.
    ///
    /// Uses per-file replace for small change sets: clones the existing
    /// in-memory snapshot, removes old nodes/edges for the changed files
    /// via [`atlas_engine::GraphSnapshot::replace_files_in_place`], then
    /// merges the fresh data from the store.  For large change sets
    /// (> 500 files), falls back to full rebuild (cloning the snapshot
    /// becomes costlier than SQLite scan).
    pub(crate) fn refresh_graph_for_files(
        &mut self,
        store: &Store,
        file_ids: &[FileId],
    ) -> anyhow::Result<()> {
        if !self.graph_initialized || file_ids.is_empty() {
            return Ok(());
        }

        const REPLACE_THRESHOLD: usize = 500;
        if file_ids.len() > REPLACE_THRESHOLD {
            // Force full rebuild — caller must also invalidate cache
            let current = store.index_signature().unwrap_or_default();
            if current != self.last_graph_signature {
                tracing::info!("Index signature changed, refreshing graph (large batch fallback)");
                let graph = Arc::new(GraphEngine::from_store(store, 0.3)?);
                self.swap_graph(store, graph);
            }
            return Ok(());
        }

        // Clone the existing snapshot and replace changed files in-place
        let old_graph = match self.search.as_ref() {
            Some(s) => s.graph_snapshot(),
            None => {
                // No existing graph — fall through to full rebuild via signature check
                let current = store.index_signature().unwrap_or_default();
                if current != self.last_graph_signature {
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
        if let Some(ref mut s) = self.search {
            s.refresh_graph(Arc::clone(&new_graph));
        }
        if let Some(ref mut c) = self.context {
            c.refresh_graph(new_graph);
        }

        self.last_graph_signature = store.index_signature().unwrap_or_default();

        Ok(())
    }
}

// ── Clone ──────────────────────────────────────────────────────────────

/// Manual `Clone` impl: `SearchEngine` and `ContextBuilder` contain
/// `RwLock` and are not `Clone`.  We only clone `GraphState` before
/// initialization (when both are `None`), so the clone always resets
/// the engines to `None` and shares the background-rebuild `Arc`.
impl Clone for GraphState {
    fn clone(&self) -> Self {
        Self {
            search: None,
            context: None,
            graph_initialized: self.graph_initialized,
            last_graph_signature: self.last_graph_signature.clone(),
            pending_graph_rebuild: Arc::clone(&self.pending_graph_rebuild),
        }
    }
}

impl GraphProvider for GraphState {
    fn is_initialized(&self) -> bool {
        self.graph_initialized
    }

    fn search_engine(&self) -> Option<&atlas_engine::SearchEngine> {
        self.search.as_ref()
    }

    fn context_builder(&self) -> Option<&atlas_engine::ContextBuilder> {
        self.context.as_ref()
    }

    fn node_count(&self) -> usize {
        self.search
            .as_ref()
            .map(|s| s.graph_snapshot().node_count())
            .unwrap_or(0)
    }

    fn edge_count(&self) -> usize {
        self.search
            .as_ref()
            .map(|s| s.graph_snapshot().edge_count())
            .unwrap_or(0)
    }
}
