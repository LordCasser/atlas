//! Graph session: owns the installed `GraphEngine` and `ContextBuilder` used
//! by the main TUI thread. Snapshot construction happens in `JobManager`.
//!
//! Analogous to `ToolRouter`'s graph management in `atlas-mcp`, but
//! decoupled from MCP protocol concerns.

use atlas_engine::{ContextBuilder, GraphEngine, SourceExtractor, Store};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Manages the in-memory graph snapshot and context builder for one project.
pub struct GraphSession {
    store: Arc<Store>,
    graph: Option<Arc<GraphEngine>>,
    context: Option<ContextBuilder>,
    initialized: bool,
    stale_flag: Arc<AtomicBool>,
    project_root: PathBuf,
}

impl GraphSession {
    /// Create a new session without building the graph.
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            graph: None,
            context: None,
            initialized: false,
            stale_flag: Arc::new(AtomicBool::new(false)),
            project_root,
        }
    }

    /// Notify the session that the database has been written to externally
    /// (e.g. by lazy structural extraction), so the next graph-backed action
    /// submits a fresh `LoadGraph` job.
    pub fn mark_stale(&self) {
        self.stale_flag.store(true, Ordering::Release);
    }

    /// Whether a graph-backed action should reload the snapshot before use.
    pub fn needs_refresh(&self) -> bool {
        self.stale_flag.load(Ordering::Acquire)
    }

    /// Install a graph snapshot built off the UI thread and rebuild only the
    /// lightweight derived query helpers on the caller thread.
    pub fn install_graph(&mut self, graph: Arc<GraphEngine>) {
        let source_extractor =
            SourceExtractor::new(Arc::clone(&self.store), self.project_root.clone());
        let context = ContextBuilder::new(Arc::clone(&self.store), Arc::clone(&graph))
            .with_project_root(self.project_root.clone())
            .with_source_fn(Arc::new(source_extractor));

        self.graph = Some(graph);
        self.context = Some(context);
        self.initialized = true;
        self.stale_flag.store(false, Ordering::Release);
    }

    // ── accessors (panic if not initialized) ──────────────────────────────

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn context_builder(&self) -> &ContextBuilder {
        self.context.as_ref().expect("graph not installed")
    }

    /// Access the low-level [`GraphEngine`] for graph traversal queries.
    pub fn graph_engine(&self) -> &Arc<GraphEngine> {
        self.graph.as_ref().expect("graph not installed")
    }
}
