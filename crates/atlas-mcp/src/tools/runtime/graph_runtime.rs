//! Graph runtime — in-memory call-graph snapshot lifecycle.
//!
//! # Responsibilities
//! - Lazy graph initialization on first graph-backed tool call
//! - AnswerQuality mode detection (RepoCanonical vs FocusScoped)
//! - Incremental graph refresh after lazy extraction writes
//! - Exposes SearchEngine (BFS/DFS/path) and ContextBuilder (callers/callees/source)
//! - Generation-based staleness detection via RuntimeInvalidation
//!
//! # Public API
//! - `ensure_initialized()`: build graph snapshot from DB (idempotent)
//! - `provider()`: return &dyn GraphProvider — sole entry point for graph queries
//! - `is_graph_stale()` / `mark_graph_fresh()`: generation-based staleness tracking
//!
//! # Usage pattern
//! ```ignore
//! self.active.graph_runtime.ensure_initialized()?;
//! let cb = self.active.graph_runtime.provider().context_builder();
//! ```
//!
//! # Dependencies
//! - `atlas_engine::{GraphEngine, SearchEngine, ContextBuilder, GraphSnapshot}`
//! - `super::graph_provider::GraphProvider`
//! - `super::invalidation::RuntimeInvalidation`

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atlas_engine::{SourceExtractor, Store};

use super::closure_graph_provider::ClosureGraphProvider;
use super::graph_provider::GraphProvider;
use super::graph_state::GraphState;
use super::invalidation::RuntimeInvalidation;

// ── Edge provenance (architecture §1.1 L3) ──────────────────────────────

/// Where in-memory graph edges are sourced from (architecture: EdgeProvenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeProvenance {
    /// Full-repo cache: edges are canonical, high-confidence (`RepoCanonical`).
    RepoCanonical,
    /// Focus/closure: edges may be partial; graph may mix canonical and closure edges.
    FocusScoped,
}

// ── GraphRuntime ────────────────────────────────────────────────────────

/// Manages the in-memory call graph snapshot lifecycle.
///
/// Provides lazy initialization and incremental refresh of the
/// SearchEngine and ContextBuilder backed by GraphState.
pub struct GraphRuntime {
    /// Shared state used by both graph providers.
    pub state: Arc<GraphState>,
    /// Closure-scoped provider that shares `state`.
    closure_provider: ClosureGraphProvider,
    pub store: Arc<Store>,
    pub source_extractor: SourceExtractor,
    pub project_root: PathBuf,
    /// Provenance mode of graph edges (detected on first init).
    pub provenance: Mutex<EdgeProvenance>,
    /// Cached store index mode at graph init time.
    cached_catalog_tier: Mutex<Option<String>>,
    /// Shared invalidation counters for generation-based staleness detection.
    pub invalidation: Arc<RuntimeInvalidation>,
    /// Cached graph_generation at last refresh — compared against current
    /// to decide if a full rebuild is needed.
    pub last_graph_generation: AtomicU64,
}

impl GraphRuntime {
    pub fn new(
        store: Arc<Store>,
        source_extractor: SourceExtractor,
        project_root: PathBuf,
        invalidation: Arc<RuntimeInvalidation>,
    ) -> Self {
        let last_graph_signature = store.index_signature().unwrap_or_default();
        let last_graph_generation = invalidation.graph_generation.load(Ordering::Relaxed);
        let state = Arc::new(GraphState {
            search: Mutex::new(None),
            context: Mutex::new(None),
            graph_initialized: std::sync::atomic::AtomicBool::new(false),
            last_graph_signature: Mutex::new(last_graph_signature),
            pending_graph_rebuild: Arc::new(std::sync::Mutex::new(None)),
        });
        let closure_provider = ClosureGraphProvider::new(Arc::clone(&state));
        Self {
            state,
            closure_provider,
            store,
            source_extractor,
            project_root,
            provenance: Mutex::new(EdgeProvenance::FocusScoped),
            cached_catalog_tier: Mutex::new(None),
            invalidation,
            last_graph_generation: AtomicU64::new(last_graph_generation),
        }
    }

    /// Ensure the graph is initialized (lazy init on first query).
    /// Detects and caches the graph provenance mode on first init.
    /// Returns &SearchEngine or an error.
    pub fn ensure_initialized(&self) -> anyhow::Result<()> {
        let was_initialized = self
            .state
            .graph_initialized
            .load(std::sync::atomic::Ordering::Acquire);
        self.state
            .ensure_initialized(&self.store, &self.source_extractor, &self.project_root)?;
        if !was_initialized {
            let store = self.store.clone();
            self.detect_and_set_mode(&store);
        }
        if !self.state.is_initialized() {
            return Err(anyhow::anyhow!("graph not initialized"));
        }
        Ok(())
    }

    /// Returns true if the graph needs a full rebuild (generation changed).
    pub(crate) fn is_graph_stale(&self) -> bool {
        let current = self.invalidation.graph_generation.load(Ordering::Relaxed);
        current > self.last_graph_generation.load(Ordering::Relaxed)
    }

    /// Mark the graph as fresh (update cached generation to match current).
    pub(crate) fn mark_graph_fresh(&self) {
        self.last_graph_generation.store(
            self.invalidation.graph_generation.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    /// Detect the index precision mode from the store and cache it.
    pub fn detect_and_set_mode(&self, store: &Store) {
        let catalog_tier = store.read_catalog_tier().unwrap_or_default();
        *self.cached_catalog_tier.lock().unwrap() = Some(catalog_tier.clone());
        *self.provenance.lock().unwrap() = if atlas_engine::has_finalized_repo_cache_for(
            store,
            atlas_engine::QueryNeed::CallGraph,
        ) {
            EdgeProvenance::RepoCanonical
        } else {
            EdgeProvenance::FocusScoped
        };
    }

    /// Returns the graph provider for the current scope.
    ///
    /// Dispatches based on [`EdgeProvenance`]:
    /// - `RepoCanonical` → `&self.state` (full graph)
    /// - `FocusScoped` → `&self.closure_provider` (closure-scoped)
    pub(crate) fn provider(&self) -> &dyn GraphProvider {
        match *self.provenance.lock().unwrap() {
            EdgeProvenance::RepoCanonical => &*self.state,
            EdgeProvenance::FocusScoped => &self.closure_provider,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{FactCoverage, FileId, FileInfo, Language, ParseStatus, Store};
    use std::sync::Arc;

    fn create_test_graph_runtime() -> GraphRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let source_extractor = SourceExtractor::new(store.clone(), PathBuf::from("."));
        let invalidation = Arc::new(RuntimeInvalidation::new());
        GraphRuntime::new(store, source_extractor, PathBuf::from("."), invalidation)
    }

    fn insert_complete_structural_file(store: &Store) {
        let file_id = FileId::generate("src/main.ts");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/main.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                "hash",
                "complete",
                FactCoverage::default(),
            )
            .unwrap();
    }

    fn mark_finalized_index(store: &Store, grade: &str, include_patterns: &[&str]) {
        store.set_metadata("last_index_time", "1").unwrap();
        store
            .set_metadata(
                "indexed_scope",
                &serde_json::json!({
                    "include": include_patterns,
                    "exclude": [],
                })
                .to_string(),
            )
            .unwrap();
        store.set_metadata("indexed_pipeline_grade", grade).unwrap();
    }

    #[test]
    fn default_mode_is_focus_scoped() {
        let gr = create_test_graph_runtime();
        assert_eq!(*gr.provenance.lock().unwrap(), EdgeProvenance::FocusScoped);
    }

    #[test]
    fn detect_and_set_mode_respects_store() {
        let gr = create_test_graph_runtime();
        // Clone store to avoid simultaneous mutable+immutable borrow.
        let store = gr.store.clone();
        gr.detect_and_set_mode(&store);
        assert_eq!(*gr.provenance.lock().unwrap(), EdgeProvenance::FocusScoped);
    }

    #[test]
    fn unfinalized_structural_facts_are_focus_scoped() {
        let gr = create_test_graph_runtime();
        insert_complete_structural_file(&gr.store);
        let store = gr.store.clone();
        gr.detect_and_set_mode(&store);

        assert_eq!(*gr.provenance.lock().unwrap(), EdgeProvenance::FocusScoped);
    }

    #[test]
    fn whole_repo_structural_index_is_repo_canonical() {
        let gr = create_test_graph_runtime();
        insert_complete_structural_file(&gr.store);
        mark_finalized_index(&gr.store, "structural", &[]);
        let store = gr.store.clone();
        gr.detect_and_set_mode(&store);

        assert_eq!(
            *gr.provenance.lock().unwrap(),
            EdgeProvenance::RepoCanonical
        );
    }

    #[test]
    fn focus_enrichment_cannot_promote_manifest_index_provenance() {
        let gr = create_test_graph_runtime();
        insert_complete_structural_file(&gr.store);
        mark_finalized_index(&gr.store, "manifest", &[]);
        let store = gr.store.clone();
        gr.detect_and_set_mode(&store);

        assert_eq!(*gr.provenance.lock().unwrap(), EdgeProvenance::FocusScoped);
    }

    #[test]
    fn scoped_structural_index_is_focus_scoped() {
        let gr = create_test_graph_runtime();
        insert_complete_structural_file(&gr.store);
        mark_finalized_index(&gr.store, "structural", &["src/**"]);
        let store = gr.store.clone();
        gr.detect_and_set_mode(&store);

        assert_eq!(*gr.provenance.lock().unwrap(), EdgeProvenance::FocusScoped);
    }

    #[test]
    fn ensure_initialized_sets_up_search_engine() {
        let gr = create_test_graph_runtime();
        let result = gr.ensure_initialized();
        assert!(result.is_ok(), "ensure_initialized should succeed");
        let gs_opt = gr.provider().graph_snapshot();
        assert!(
            gs_opt.is_some(),
            "graph_snapshot should be accessible after init"
        );
    }

    #[test]
    fn provider_trait_contract_holds() {
        let gr = create_test_graph_runtime();
        {
            let p = gr.provider();
            assert!(!p.is_initialized());
            assert!(p.graph_snapshot().is_none());
            assert!(p.graph_snapshot().is_none());
        }

        gr.ensure_initialized().unwrap();
        let p = gr.provider();
        assert!(p.is_initialized());
        // Using an empty store yields zero nodes/edges — this is expected.
    }

    #[test]
    fn graph_not_stale_after_construction() {
        let gr = create_test_graph_runtime();
        assert!(!gr.is_graph_stale());
    }

    #[test]
    fn graph_stale_after_bump() {
        let gr = create_test_graph_runtime();
        let initial_gen = gr.last_graph_generation.load(Ordering::Relaxed);
        gr.invalidation
            .graph_generation
            .fetch_add(1, Ordering::Relaxed);
        assert!(gr.is_graph_stale());
        gr.mark_graph_fresh();
        assert!(!gr.is_graph_stale());
        assert!(gr.last_graph_generation.load(Ordering::Relaxed) > initial_gen);
    }
}
