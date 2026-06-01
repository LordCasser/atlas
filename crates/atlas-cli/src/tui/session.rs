//! Graph session: manages the lifecycle of `GraphEngine`, `SearchEngine`,
//! and `ContextBuilder` with lazy initialisation and signature-based refresh.
//!
//! Analogous to `ToolRouter`'s graph management in `atlas-mcp`, but
//! decoupled from MCP protocol concerns.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use atlas_engine::{ContextBuilder, GraphEngine, SearchEngine, Store};

/// Manages the in-memory graph snapshot and derived engines for a single
/// project session.  The graph is loaded lazily on first use and refreshed
/// when the SQLite database signature changes.
pub struct GraphSession {
    store: Arc<Store>,
    graph: Option<Arc<GraphEngine>>,
    search: Option<SearchEngine>,
    context: Option<ContextBuilder>,
    last_signature: String,
    initialized: bool,
    stale_flag: Arc<AtomicBool>,
    last_check: Instant,
    project_root: PathBuf,
}

impl GraphSession {
    /// Create a new session without building the graph.
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            graph: None,
            search: None,
            context: None,
            last_signature: String::new(),
            initialized: false,
            stale_flag: Arc::new(AtomicBool::new(false)),
            last_check: Instant::now(),
            project_root,
        }
    }

    // ── lifecycle ─────────────────────────────────────────────────────────

    /// Build the graph on first use (idempotent).
    ///
    /// Subsequent calls are a no-op.  Use [`maybe_refresh`] or
    /// [`force_refresh`] to pick up database changes after initialisation.
    pub fn ensure_initialized(&mut self) -> anyhow::Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.rebuild().context("Failed to build graph snapshot")
    }

    /// Refresh the graph if the database signature has changed.
    ///
    /// Cached for 5 seconds to avoid per-request `COUNT` queries.
    /// If [`mark_stale`] was called (e.g. after lazy structural extraction),
    /// the cooldown is skipped.
    pub fn maybe_refresh(&mut self) -> anyhow::Result<()> {
        if !self.initialized {
            return Ok(());
        }

        // Background lazy structural may have written new facts —
        // skip cooldown to pick them up immediately.
        if self.stale_flag.swap(false, Ordering::AcqRel) {
            self.last_check = self
                .last_check
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or(self.last_check);
        }

        if self.last_check.elapsed().as_secs() < 5 {
            return Ok(());
        }

        self.last_check = Instant::now();
        let current = self
            .store
            .index_signature()
            .unwrap_or_default();
        if current != self.last_signature {
            tracing::info!(
                "Index signature changed, refreshing graph (was: {}, now: {})",
                self.last_signature,
                current
            );
            self.rebuild()?;
        }
        Ok(())
    }

    /// Rebuild the graph unconditionally, regardless of signature.
    ///
    /// Called after `AutoIndex` completes so the fresh database is loaded
    /// immediately.
    pub fn force_refresh(&mut self) -> anyhow::Result<()> {
        self.rebuild()
            .context("Failed to force-refresh graph snapshot")
    }

    /// Notify the session that the database has been written to externally
    /// (e.g. by lazy structural extraction), so the next `maybe_refresh`
    /// should skip its cooldown.
    pub fn mark_stale(&self) {
        self.stale_flag.store(true, Ordering::Release);
    }

    // ── accessors (panic if not initialized) ──────────────────────────────

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn search_engine(&self) -> &SearchEngine {
        self.search
            .as_ref()
            .expect("graph not initialized; call ensure_initialized() first")
    }

    pub fn context_builder(&self) -> &ContextBuilder {
        self.context
            .as_ref()
            .expect("graph not initialized; call ensure_initialized() first")
    }

    pub fn engine(&self) -> &Arc<GraphEngine> {
        self.graph
            .as_ref()
            .expect("graph not initialized; call ensure_initialized() first")
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    // ── internal ──────────────────────────────────────────────────────────

    fn rebuild(&mut self) -> anyhow::Result<()> {
        tracing::info!("Building graph snapshot...");
        let start = Instant::now();

        let graph = Arc::new(
            GraphEngine::from_store(&self.store, 0.3)
                .context("Failed to load graph from database")?,
        );

        let search = SearchEngine::new(Arc::clone(&self.store), Arc::clone(&graph));
        let context = ContextBuilder::new(Arc::clone(&self.store), Arc::clone(&graph))
            .with_project_root(self.project_root.clone());

        self.last_signature = self.store.index_signature().unwrap_or_default();
        self.graph = Some(graph);
        self.search = Some(search);
        self.context = Some(context);
        self.initialized = true;

        let elapsed = start.elapsed();
        tracing::info!(
            "Graph snapshot ready ({:.1}s, sig: {})",
            elapsed.as_secs_f64(),
            self.last_signature,
        );
        Ok(())
    }
}
