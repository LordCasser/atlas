//! Atlas Engine — unified facade crate for extraction, resolution, graph, and trace.
//!
//! # Architecture
//!
//! `atlas-engine` is the stable public API for Atlas.  It re-exports all core
//! types from [`atlas_types`] and provides a high-level [`Engine`] struct that
//! wraps the full pipeline:
//!
//! ```text
//!   parse → extract → resolve → build-graph → trace
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use atlas_engine::Engine;
//!
//! let engine = Engine::open_in_memory()?;
//! engine.extract_file("test.ts", "function f() {}")?;
//! ```
//!
//! # Feature flags
//!
//! Language support is gated by Cargo features (e.g. `typescript`, `python`,
//! `java`).  The `all-languages` feature enables every language except
//! experimental frontends (Cangjie).

use std::path::Path;
use std::sync::Arc;

// lazy crate (aliased to avoid name conflict with types::lazy module)
use ::lazy as lazy_crate;

/// Lazy dataflow service: planner + loader for on-demand dataflow.
pub use lazy_crate::LazyDataflowService;

// ── Internal modules ──────────────────────────────────────────────────────

mod closure_planner;
/// Investigation context: MCP-session-scoped analysis focus for lazy job prioritization.
pub mod investigation;
mod lazy_coordinator;
mod lazy_structural;
mod linux_augment;
/// Precision tier computation for lazy extraction transparency.
pub mod precision;
mod source_extractor;
mod lazy_budget;
mod lazy_orchestrator;

/// Closure planner: dependency-closure-aware lazy extraction planning.
pub use closure_planner::{ClosurePlanner, DependencyClosure, IncludeRoot, PrioritizedWorkset};

/// Lazy coordinator: orchestrates lazy extraction with job tracking and in-flight dedup.
pub use lazy_coordinator::LazyCoordinator;

/// Lazy structural service: on-demand full structural extraction.
pub use lazy_structural::{
    CandidateProvider, DefaultCandidateProvider, EnsureStructuralResult, LazyStructuralService,
};

/// Source extraction: AST-based symbol source retrieval.
pub use source_extractor::SourceExtractor;

// ─── Re-exports ────────────────────────────────────────────────────────────

/// Analysis: lifecycle and branch diff engines.
pub use analysis;
/// Analysis: domain rules, lifecycle proof, and rule learning.
pub use analysis::domain_rules;
pub use analysis::lifecycle_proof;
pub use analysis::rule_learning;
/// Analysis: C/C++ ownership rules consumer.
pub use analysis::ownership_rules::CppOwnershipRules;
/// Domain rules: language-agnostic rule engine.
pub use domain_rules as rule_engine;
/// Analysis: summary builder.
pub use analysis::summary::SummaryBuilder;
/// Analysis layer: trace engine and query responses.
///
/// [`RawTraceEngine`] is the low-level analysis engine — it does NOT
/// automatically trigger lazy dataflow loading.  Callers must run
/// [`LazyDataflowService::ensure_for_position`] first, or use the
/// high-level [`Engine::trace_variable`] which wraps both.
pub use analysis::trace;
pub use analysis::trace::TraceEngine as RawTraceEngine;
pub use analysis::trace::TraceQueryResponse;
/// Context layer: AI context builder (callers, callees, peers).
pub use context::{CalleeDetail, CallerDetail, ContextBuilder, ContextView};
/// Summary persistence: build and query function summaries.
pub use db::summary::{SummaryBuildStats, SummaryStore};
/// Database store and schema version.
pub use db::{CURRENT_SCHEMA_VERSION, Store};
/// Extraction layer: language frontends, parser pool, grammar registry.
pub use extraction::{
    ExtractionMode, LanguageFrontend, LanguageRegistry, ParseWorkerPool, WorkerConfig,
    create_frontend, extract_file, extract_file_with_mode, extract_file_with_mode_cancellable,
};
/// Sync layer: incremental sync engine, file lock, file discovery.
pub use filesync::{
    DirtySet, ExtractedFile, ExtractedFiles, ExtractionPhaseStats, FileLock, GraphResult,
    IndexPipelineOptions, IndexPipelineStats, IndexProgress, IndexProgressCallback, SyncEngine,
    SyncStats, WriteBatchStats, build_dirty_set, clean_stale_file_ids, clean_stale_file_paths,
    discovery, phase_build_summaries, phase_cleanup_file_ids, phase_cleanup_stale,
    phase_commit_path_alias_config, phase_dirty_check, phase_discover, phase_extract_serial,
    phase_finalize, phase_init_frontends, phase_materialize_annotations, phase_resolve_and_build,
    phase_write_batched, phase_write_single, run_index_pipeline, source_file_id,
};
/// Graph layer: graph builder, query engine, snapshots, annotation materialization.
pub use graph::{
    CallGraphView, CompositePathScore, ForwardFrontier, FrontierNode, GraphBuilder,
    GraphBuilderStats, GraphEngine, GraphPath, GraphSnapshot, NodeIx, PathBreakpoint,
    PathBreakpointKind, PathEdge, PathEdgeDirection, RankedPath, Subgraph, TraversalConfig,
    TraversalDirection, materialize_annotations,
};
/// Resolution layer: reference resolver, path aliases, config hashing.
pub use resolution::{
    PATH_ALIAS_CONFIG_FILES, PathAliasConfig, PathAliasResolver, ReferenceResolver,
    ResolutionSession, ResolutionStats, commit_config_hashes, detect_config_change,
};
/// Search layer: FTS5 + fuzzy search engine.
pub use search::{SearchEngine, SearchOptions, SearchResult};
/// Progress protocol (for CLI TUI integration).
pub use types::progress;
/// All core IR types (SymbolDef, ReferenceUse, FileFacts, etc.).
pub use types::*;
/// Workspace abstractions.
pub use workspace::{ProjectRoot, SourcePath, Workspace};
/// Investigation context types: focus, related symbols/files, desired capabilities.
pub use investigation::{Investigation, InvestigationFocus};
/// Unified lazy extraction orchestration: policy presets, outcomes, orchestrator.
pub use lazy_orchestrator::{LazyOrchestrator, LazyOutcome, LazyPolicy};

// ─── Engine ────────────────────────────────────────────────────────────────

/// High-level Atlas engine wrapping the full extraction → resolution → trace pipeline.
///
/// # Lifecycle
///
/// ```text
///   open() / open_in_memory()
///     └── extract_file()          # parse + extract FileFacts
///     └── language_capability()   # query language profiles
///     └── trace_variable()        # backward dataflow trace
///     └── trace_callers()         # reverse call-graph exploration
///     └── trace_point()           # position-to-context resolution
/// ```
pub struct Engine {
    store: Arc<Store>,
    lazy_service: lazy_crate::LazyDataflowService,
    lazy_structural: LazyStructuralService,
    trace: analysis::trace::TraceEngine,
}

impl Engine {
    // ── Constructors ───────────────────────────────────────────────────

    /// Open an existing database file.
    ///
    /// The database must have been created by `atlas init` or a prior run.
    /// The schema is NOT initialized here — that is the CLI's responsibility.
    ///
    /// **Limitation**: Opens without a project root.  Lazy dataflow/CFG
    /// extraction that reads source files from disk requires the DB to be
    /// opened from the project root directory (sources are stored as relative
    /// paths).  Use [`Engine::open_with_root`] when the caller is not running
    /// from the project root or when sources are stored as absolute paths.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let store = Store::open_db(db_path)?;
        let store = Arc::new(store);
        let lazy_service = lazy_crate::LazyDataflowService::new(store.clone(), None);
        let lazy_structural = LazyStructuralService::new(store.clone(), None);
        let trace = analysis::trace::TraceEngine::new(store.clone());
        Ok(Self {
            store,
            lazy_service,
            lazy_structural,
            trace,
        })
    }

    /// Open a database file with a project root for snippet extraction.
    ///
    /// When a project root is provided, trace results can include source
    /// code snippets from the file system.
    pub fn open_with_root(db_path: &Path, project_root: &Path) -> anyhow::Result<Self> {
        let store = Store::open_db(db_path)?;
        let store = Arc::new(store);
        let lazy_service =
            lazy_crate::LazyDataflowService::new(store.clone(), Some(project_root.to_path_buf()));
        let lazy_structural =
            LazyStructuralService::new(store.clone(), Some(project_root.to_path_buf()));
        let trace =
            analysis::trace::TraceEngine::new_with_root(store.clone(), project_root.to_path_buf());
        Ok(Self {
            store,
            lazy_service,
            lazy_structural,
            trace,
        })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let store = Arc::new(store);
        let lazy_service = lazy_crate::LazyDataflowService::new(store.clone(), None);
        let lazy_structural = LazyStructuralService::new(store.clone(), None);
        let trace = analysis::trace::TraceEngine::new(store.clone());
        Ok(Self {
            store,
            lazy_service,
            lazy_structural,
            trace,
        })
    }

    /// Access the underlying database store.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Access the underlying trace engine.
    pub fn trace_engine(&self) -> &analysis::trace::TraceEngine {
        &self.trace
    }

    /// Access the lazy structural service for on-demand extraction.
    pub fn lazy_structural(&self) -> &LazyStructuralService {
        &self.lazy_structural
    }

    // ── Extraction ─────────────────────────────────────────────────────

    /// Extract facts from a single source file.
    ///
    /// Uses [`ExtractionMode::Full`] by default for backward compatibility.
    /// For index-time usage, prefer the mode-aware variant.
    pub fn extract_file(
        &self,
        path: &Path,
        source: &str,
        language: Language,
    ) -> anyhow::Result<FileFacts> {
        self.extract_file_with_mode(path, source, language, extraction::ExtractionMode::Full)
    }

    /// Extract facts from a single source file with explicit mode control.
    pub fn extract_file_with_mode(
        &self,
        path: &Path,
        source: &str,
        language: Language,
        mode: extraction::ExtractionMode,
    ) -> anyhow::Result<FileFacts> {
        let frontend = extraction::create_frontend(language)
            .ok_or_else(|| anyhow::anyhow!("Language frontend not available for {:?}", language))?;
        let file_id = FileId::generate(path.to_string_lossy().as_ref());
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let facts = extraction::extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            &content_hash,
            mode,
        )?;
        Ok(facts)
    }

    /// Insert extracted facts into the database.
    pub fn insert_facts(&self, facts: &FileFacts) -> anyhow::Result<()> {
        self.store.insert_file_facts(facts)?;
        Ok(())
    }

    // ── Capability ─────────────────────────────────────────────────────

    /// Get the capability profile for a language.
    pub fn language_capability(language: Language) -> LanguageCapabilityProfile {
        LanguageCapabilityProfile::for_language(language)
    }

    /// Get capability profiles for all compiled-in languages.
    pub fn all_capabilities() -> Vec<LanguageCapabilityProfile> {
        LanguageCapabilityProfile::all_compiled()
    }

    /// Resolve the language capability profile for a file on the fly.
    fn resolve_capability(&self, file_id: &FileId) -> Option<LanguageCapabilityProfile> {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|fi| LanguageCapabilityProfile::for_language(fi.language))
    }

    // ── Trace ──────────────────────────────────────────────────────────

    /// Resolve a source position to a full [`TracePoint`].
    ///
    /// Returns the symbol, data node, scope, and bindings at the given
    /// file/line/column.  Always available regardless of language capability.
    pub fn trace_point(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
    ) -> analysis::trace::TraceQueryResponse<TracePoint> {
        self.trace.trace_point(file_id, line, column)
    }

    /// Trace dataflow backward from a source position.
    ///
    /// Requires `DataflowBasic` capability for the language.  If the language
    /// does not support dataflow, returns a partial result with an
    /// `unsupported_language` diagnostic.
    ///
    /// Before executing the trace, this method automatically ensures that
    /// lazy dataflow has been built for the query's surrounding functions
    /// via [`LazyDataflowService::ensure_for_position`].
    pub fn trace_variable(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
        max_depth: usize,
    ) -> analysis::trace::TraceQueryResponse<TracePath> {
        // Resolve capability for gating
        let cap = self.resolve_capability(file_id);

        // Check dataflow support
        let dataflow_supported = cap
            .as_ref()
            .and_then(|c| c.features.as_ref())
            .map(|f| f.local_dataflow.is_supported())
            .unwrap_or(false);

        if !dataflow_supported {
            return TraceQueryResponse::partial(
                "trace_variable",
                TraceDiagnostic::warning("Dataflow not supported for this language")
                    .with_code("unsupported_language"),
                cap,
            );
        }

        // Lazy-load dataflow for the query window.
        // Always trigger lazy dataflow extraction — the loader internally
        // checks for pre-built data via `count_data_nodes_for_unit` and skips
        // extraction when data already exists.
        let lazy_start = std::time::Instant::now();
        let mut partial = false;
        let mut lazy_diagnostics: Vec<TraceDiagnostic> = Vec::new();
        let lazy_summary: Option<LazySummary>;
        match self.lazy_service.ensure_for_position(file_id, line, column, None) {
            Ok(window) => {
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: window.units_built,
                    units_cached: window.units_cached,
                    units_pending: window.units_pending,
                    pending_job_ids: window.pending_job_ids.clone(),
                    truncated: window.truncated,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                    precision_tier: window.precision_tier.clone(),
                });
                if window.truncated {
                    partial = true;
                    lazy_diagnostics.push(
                        TraceDiagnostic::warning(
                            "Lazy dataflow reached its internal budget. Result is partial. For full offline coverage, run `atlas index --analysis full`."
                        ).with_code("lazy_dataflow_budget_exceeded")
                    );
                }
                if window.units_pending > 0 {
                    partial = true;
                    lazy_diagnostics.push(
                        TraceDiagnostic::warning(
                            "Lazy dataflow is already being built by another request. Result may be partial; retry after the reported pending job completes."
                        )
                        .with_code("lazy_dataflow_already_building"),
                    );
                }
            }
            Err(e) => {
                partial = true;
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: 0,
                    units_cached: 0,
                    units_pending: 0,
                    pending_job_ids: Vec::new(),
                    truncated: true,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                    precision_tier: None,
                });
                lazy_diagnostics.push(
                    TraceDiagnostic::warning(&format!("Lazy dataflow build failed: {e}"))
                        .with_code("lazy_dataflow_build_failed"),
                );
            }
        }

        // Delegate to analysis TraceEngine
        let mut resp = self.trace.trace_variable(file_id, line, column, max_depth);
        resp.partial_result = resp.partial_result || partial;
        resp.diagnostics.extend(lazy_diagnostics);
        if let Some(ref mut path) = resp.result {
            path.lazy_summary = lazy_summary;
        }
        resp
    }

    /// Trace the call chain backward from a target symbol.
    ///
    /// Returns the caller chain from the target upward.  Requires `call_graph`
    /// support in the language's capability profile.
    pub fn trace_callers(
        &self,
        target_id: &SymbolId,
        max_depth: usize,
    ) -> analysis::trace::TraceQueryResponse<types::caller_path::CallerChain> {
        self.trace.trace_callers(target_id, max_depth)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_constructs_in_memory() {
        let engine = Engine::open_in_memory().expect("should create in-memory engine");
        let _ = engine.store();
        let _ = engine.trace_engine();
    }

    #[test]
    fn engine_all_capabilities_not_empty() {
        let caps = Engine::all_capabilities();
        // At minimum, typescript should be in the default features
        assert!(
            !caps.is_empty(),
            "all_capabilities should return at least one profile with default features"
        );
    }

    #[test]
    #[cfg(feature = "typescript")]
    fn engine_capability_for_typescript() {
        let cap = Engine::language_capability(Language::TypeScript);
        assert_eq!(cap.language, "typescript");
        assert!(
            cap.capability_level >= types::capability::CapabilityLevel::DataflowBasic,
            "TypeScript should be at least DataflowBasic"
        );
    }

    #[test]
    fn engine_open_in_memory_has_empty_store() {
        let engine = Engine::open_in_memory().expect("should create in-memory engine");
        let stats = engine.store().get_stats().expect("should get stats");
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_symbols, 0);
    }

    #[test]
    #[cfg(feature = "typescript")]
    fn engine_extract_and_insert_typescript_smoke() {
        let engine = Engine::open_in_memory().expect("should create in-memory engine");
        let source = "function add(a: number, b: number) {\n  return a + b;\n}\nadd(1, 2);\n";
        let file_path = Path::new("test.ts");

        let facts = engine
            .extract_file(file_path, source, Language::TypeScript)
            .expect("should extract TS file");

        assert!(!facts.symbols.is_empty(), "should have symbols");
        assert!(!facts.references.is_empty(), "should have references");
        assert_eq!(facts.file.language, Language::TypeScript);

        engine.insert_facts(&facts).expect("should insert facts");

        let stats = engine.store().get_stats().expect("should get stats");
        assert_eq!(stats.total_files, 1);
        assert!(stats.total_symbols > 0);
        assert!(stats.total_references > 0);
    }

    #[test]
    fn trace_query_response_serializes() {
        let resp: analysis::trace::TraceQueryResponse<&str> =
            analysis::trace::TraceQueryResponse::err("trace_point", "file not found");
        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains(r#""ok""#));
        assert!(json.contains(r#""kind""#));
        assert!(json.contains(r#""capability""#));
    }

    // ── Lazy dataflow integration tests ─────────────────────────────────

    /// 7b: trace_variable triggers lazy dataflow build automatically.
    /// Index with Structural mode (no dataflow), then trace_variable
    /// should trigger the build and produce a trace path.
    #[test]
    #[cfg(feature = "typescript")]
    fn trace_variable_triggers_lazy_dataflow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = {
            let db_path = dir.path().join(".atlas/atlas.db");
            std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
            Engine::open(&db_path).expect("open engine")
        };
        // Initialize schema — Engine::open doesn't init schema
        engine.store().init_schema().expect("init schema");

        let source = "function compute(x: number): number {\n  let y = x * 2;\n  return y;\n}\n";
        let rel_path = "test_lazy.ts";
        let abs_path = dir.path().join(rel_path);
        std::fs::write(&abs_path, source).expect("write source");

        let file_id = FileId::generate(rel_path);

        // Use the actual content hash so the lazy loader's stale-index
        // check passes (it compares DB hash with disk hash).
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        // Index with Structural mode directly via extract_file_with_mode
        let frontend = extraction::create_frontend(Language::TypeScript).unwrap();
        let facts = extraction::extract_file_with_mode(
            &frontend,
            file_id,
            &abs_path,
            source,
            &content_hash,
            extraction::ExtractionMode::Structural,
        )
        .expect("extract structural");
        engine.insert_facts(&facts).expect("insert structural");

        // Verify no dataflow exists before trace
        let dn_before = engine.store().find_data_nodes_by_file(&file_id).unwrap();
        assert!(dn_before.is_empty(), "no data nodes before lazy load");

        // trace_variable triggers lazy load
        // Line 2, column 5 is inside `let y = x * 2;` — the variable `y`
        let _resp = engine.trace_variable(&file_id, 2, 5, 10);

        // After trace_variable, data_nodes should exist in DB
        let dn_after = engine.store().find_data_nodes_by_file(&file_id).unwrap();
        assert!(
            !dn_after.is_empty(),
            "data nodes should exist after trace_variable triggers lazy load"
        );
    }

    /// 7c: Cache hit — second trace_variable call should reuse lazy dataflow
    /// without re-extracting.
    #[test]
    #[cfg(feature = "typescript")]
    fn lazy_dataflow_cache_hit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join(".atlas/atlas.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let engine = Engine::open(&db_path).expect("open engine");
        engine.store().init_schema().expect("init schema");

        let source = "function mul(a: number, b: number): number {\n  return a * b;\n}\n";
        let rel_path = "test_cache.ts";
        let abs_path = dir.path().join(rel_path);
        std::fs::write(&abs_path, source).expect("write source");

        let file_id = FileId::generate(rel_path);

        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        // Index structurally
        let frontend = extraction::create_frontend(Language::TypeScript).unwrap();
        let facts = extraction::extract_file_with_mode(
            &frontend,
            file_id,
            &abs_path,
            source,
            &content_hash,
            extraction::ExtractionMode::Structural,
        )
        .expect("extract structural");
        engine.insert_facts(&facts).expect("insert");

        // First call — triggers lazy build
        let _resp1 = engine.trace_variable(&file_id, 1, 10, 10);
        let dn1 = engine.store().find_data_nodes_by_file(&file_id).unwrap();
        let count1 = dn1.len();
        assert!(count1 > 0, "should have data nodes after first call");

        // Second call — should hit cache (same data nodes, no rebuild)
        let _resp2 = engine.trace_variable(&file_id, 1, 10, 10);
        let dn2 = engine.store().find_data_nodes_by_file(&file_id).unwrap();
        assert_eq!(dn2.len(), count1, "data node count unchanged on cache hit");
    }
}
