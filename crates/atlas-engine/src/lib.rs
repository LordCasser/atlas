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
//! All 14 languages are compiled by default. Individual languages can be
//! disabled by opting out of default features (e.g. `--no-default-features`).

use std::path::Path;
use std::sync::{Arc, Mutex};

// lazy crate (aliased to avoid name conflict with types::lazy module)
use ::lazy as lazy_crate;

/// Lazy dataflow service: planner + loader for on-demand dataflow.
pub use lazy_crate::LazyDataflowService;

// ── Internal modules ──────────────────────────────────────────────────────

mod closure_planner;
/// Precision migration adapter: new Precision model → legacy PrecisionTier.
pub mod compat;
/// Focus-driven incremental analysis types: FocusSeed, FocusWindow, FocusClosure.
pub mod focus;
mod index_precision;
/// Investigation context: MCP-session-scoped analysis focus for lazy job prioritization.
pub mod investigation;
/// Unified job context: shared cancellation and progress for long-running operations.
pub mod job_context;
mod lazy_budget;
mod lazy_outcome;
mod lazy_structural;
mod linux_augment;
/// Precision tier computation for lazy extraction transparency.
pub mod precision;
/// Scoped search service: shared search orchestration with lazy structural fallback.
pub mod scoped_search;
/// Unified symbol resolution with fault-tolerant scoring.
pub mod symbol_selector;
mod source_extractor;

// ── Stable Public API ─────────────────────────────────────────────────────
// These items form the intended, stable contract of the Atlas engine.
// External consumers should rely only on these.  Breaking changes will
// be signaled by a semver bump.

/// Extraction: entry points and mode control.
pub use extraction::{
    ExtractionMode, create_frontend, extract_file, extract_file_with_mode, parse_analysis_mode,
};

/// Sync layer: core indexing pipeline and progress protocol.
pub use filesync::{
    FileLock, IndexPipeline, IndexPipelineOptions, PhaseName, ProgressEvent, ProgressSink,
    SyncEngine,
};

/// Database store and schema version.
pub use db::{CURRENT_SCHEMA_VERSION, Store};

/// Graph layer: builder, query engine, snapshots, annotation materialization.
pub use graph::{
    GraphBuilder, GraphEngine, GraphPath, GraphSnapshot, NodeIx, TraversalDirection,
    materialize_annotations,
};

/// Resolution layer: reference resolver and path aliases.
pub use resolution::{PathAliasConfig, ReferenceResolver, ResolutionStats};

/// Analysis: trace engine (low-level, without lazy dataflow).
///
/// [`RawTraceEngine`] does NOT automatically trigger lazy dataflow loading.
/// Callers must run [`LazyDataflowService::ensure_for_position`] first, or
/// use the high-level [`Engine::trace_variable`] which wraps both.
pub use analysis::trace::TraceEngine as RawTraceEngine;
/// Analysis: trace query responses.
pub use analysis::trace::TraceQueryResponse;

/// Context layer: AI context builder (callers, callees, peers).
pub use context::ContextBuilder;

/// Search layer: FTS5 + fuzzy search engine.
pub use search::{SearchEngine, SearchResult};

/// Scoped search: shared orchestration for MCP/TUI search with lazy structural fallback.
pub use scoped_search::{
    ScopedSearchRequest, ScopedSearchResponse, ScopedSearchService, SearchAnalysis, SearchCoverage,
};

/// Unified lazy extraction outcome: consumed by MCP response builders.
pub use lazy_outcome::LazyOutcome;

/// Lazy structural service: on-demand full structural extraction.
pub use lazy_structural::LazyStructuralService;

/// Source extraction: AST-based symbol source retrieval.
pub use source_extractor::SourceExtractor;

/// Investigation context types: focus, related symbols/files, desired capabilities.
pub use investigation::{Investigation, InvestigationFocus};

/// Focus-driven incremental analysis: core types.
pub use focus::types::{ClosureStrategy, FocusClosure, FocusSeed, FocusWindow, WindowBudget};
/// Focus-driven incremental analysis: closure engine.
pub use focus::engine::ClosureEngine;
/// Focus-driven incremental analysis: query intent (MCP tool request).
pub use focus::query::QueryIntent;
/// Focus-driven incremental analysis: runtime entry point.
pub use focus::runtime::{FocusResult, FocusRuntime, IndexMode};
/// Focus-driven incremental analysis: priority scheduler.
pub use focus::scheduler::{FocusPriority, FocusScheduler};
/// Focus-driven incremental analysis: visibility filter registry.
pub use focus::visibility_filter::VisibilityFilterRegistry;

/// Closure planner: include-root for dependency-closure extraction.
pub use closure_planner::IncludeRoot;

/// Index precision: guards and queries for extraction mode stability.
pub use index_precision::{guard_against_precision_downgrade, is_rich_index_mode};

/// Workspace abstractions.
pub use workspace::Workspace;

/// Progress protocol (for CLI TUI integration).
pub use types::progress;
/// All core IR types (SymbolDef, ReferenceUse, FileFacts, etc.).
pub use types::*;

// ── Internal / Prelude ───────────────────────────────────────────────────
// These items are exported for convenience of workspace-internal crates
// (CLI, MCP, TUI).  They may change between minor versions without
// notice.  External integrators should not rely on them.

/// Closure planner internals: planner, dependency closure, prioritized worksets.
pub use closure_planner::{ClosurePlanner, DependencyClosure, PrioritizedWorkset};

/// Index precision internals: mode names and downgrade detection helpers.
pub use index_precision::{
    extraction_mode_name, recommended_analysis_for, would_downgrade_index_precision,
};

/// Lazy structural internals: candidate providers and ensure-structural result.
pub use lazy_structural::{CandidateProvider, DefaultCandidateProvider, EnsureStructuralResult};

/// Analysis: lifecycle and branch diff engines (full crate re-export).
pub use analysis;
/// Analysis: domain rules, lifecycle proof, and rule learning.
pub use analysis::domain_rules;
pub use analysis::lifecycle_proof;
/// Analysis: C/C++ ownership rules consumer.
pub use analysis::ownership_rules::CppOwnershipRules;
pub use analysis::rule_learning;
/// Analysis: summary builder.
pub use analysis::summary::SummaryBuilder;
/// Analysis: trace module (for qualified access to trace sub-items).
pub use analysis::trace;

/// Context internals: caller/callee detail types and context view.
pub use context::{CalleeDetail, CallerDetail, ContextView};

/// Dossier: Symbol Dossier builder for atlas_explore tool.
pub use dossier;

/// Summary persistence internals: build stats and low-level store.
pub use db::summary::{SummaryBuildStats, SummaryStore};

/// Domain rules: language-agnostic rule engine (aliased).
pub use domain_rules as rule_engine;

/// Extraction internals: language frontends, parser pool, grammar registry.
pub use extraction::{
    LanguageFrontend, LanguageRegistry, ParseWorkerPool, WorkerConfig, available_languages,
    extract_file_with_mode_cancellable,
};

/// Sync layer internals: dirty-set tracking, phase functions, pipeline runners.
pub use filesync::{
    DirtySet, ExtractedFile, ExtractedFiles, ExtractionPhaseStats, GraphResult, IndexPipelineStats,
    IndexProgress, IndexProgressCallback, SyncStats, WriteBatchStats, build_dirty_set,
    clean_stale_file_ids, clean_stale_file_paths, discovery, phase_build_summaries,
    phase_cleanup_file_ids, phase_cleanup_stale, phase_commit_path_alias_config, phase_dirty_check,
    phase_discover, phase_extract_serial, phase_finalize, phase_init_frontends,
    phase_materialize_annotations, phase_resolve_and_build, phase_write_batched,
    phase_write_single, run_index_pipeline, source_file_id,
};

/// Graph layer internals: advanced query types and path internals.
pub use graph::{
    CallGraphView, CompositePathScore, ForwardFrontier, FrontierNode, GraphBuilderStats,
    PathBreakpoint, PathBreakpointKind, PathEdge, PathEdgeDirection, RankedPath, Subgraph,
    TraversalConfig,
};

/// Job context: shared cancellation token and progress sink for long operations.
pub use job_context::JobContext;

/// Resolution internals: path-alias resolver, session, config hashing.
pub use resolution::{
    PATH_ALIAS_CONFIG_FILES, PathAliasResolver, ResolutionSession, commit_config_hashes,
    detect_config_change,
};

pub use search::SearchOptions;
/// Search internals: query parser and options.
pub use search::query_parser::{ParsedQuery, parse_query, searchable_languages};

/// Workspace internals: project root and source path types.
pub use workspace::{ProjectRoot, SourcePath};

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
    /// Optional focus runtime — initialized via [`Engine::init_focus`].
    focus_scheduler: Option<Arc<Mutex<FocusScheduler>>>,
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
            focus_scheduler: None,
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
            focus_scheduler: None,
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
            focus_scheduler: None,
        })
    }

    /// Construct an Engine from an existing [`Arc<Store>`].
    ///
    /// Useful when the caller already holds an `Arc<Store>` (e.g., MCP server
    /// creates its own store) and wants a high-level Engine wrapped around it.
    /// When `project_root` is provided, trace results can include source code
    /// snippets from the file system.
    pub fn from_store(store: Arc<Store>, project_root: Option<&std::path::Path>) -> Self {
        let lazy_service = lazy_crate::LazyDataflowService::new(
            store.clone(),
            project_root.map(|p| p.to_path_buf()),
        );
        let lazy_structural =
            LazyStructuralService::new(store.clone(), project_root.map(|p| p.to_path_buf()));
        let trace = if let Some(root) = project_root {
            analysis::trace::TraceEngine::new_with_root(store.clone(), root.to_path_buf())
        } else {
            analysis::trace::TraceEngine::new(store.clone())
        };
        Self {
            store,
            lazy_service,
            lazy_structural,
            trace,
            focus_scheduler: None,
        }
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

    // ── Focus ───────────────────────────────────────────────────────────

    /// Initialize the focus runtime. Requires atlas open (not in-memory).
    ///
    /// Constructs a [`ClosureEngine`] and [`FocusScheduler`] wired to the
    /// Engine's store and lazy structural service.  Once initialized, the
    /// scheduler can be used for background focus closure building via
    /// [`FocusScheduler::enqueue`] and [`FocusScheduler::start_background`].
    pub fn init_focus(
        &mut self,
        project_root: &Path,
        include_roots: Vec<IncludeRoot>,
    ) -> anyhow::Result<()> {
        // Construct a LazyStructuralService for the ClosureEngine.
        // We create a new instance rather than cloning self.lazy_structural
        // because LazyStructuralService is not Clone (it owns a Box<dyn CandidateProvider>).
        let lazy_structural = LazyStructuralService::new(
            self.store.clone(),
            Some(project_root.to_path_buf()),
        );
        let lazy_dataflow = LazyDataflowService::new(
            self.store.clone(),
            Some(project_root.to_path_buf()),
        );
        let engine = ClosureEngine::new(
            self.store.clone(),
            lazy_structural,
            lazy_dataflow,
            Some(project_root.to_path_buf()),
            include_roots,
        );
        let scheduler = FocusScheduler::new(self.store.clone()).with_engine(engine);
        self.focus_scheduler = Some(Arc::new(Mutex::new(scheduler)));
        Ok(())
    }

    /// Get the focus scheduler (if initialized via [`init_focus`]).
    pub fn focus_scheduler(&self) -> Option<&Arc<Mutex<FocusScheduler>>> {
        self.focus_scheduler.as_ref()
    }

    /// Check if the focus runtime is active.
    pub fn has_focus(&self) -> bool {
        self.focus_scheduler.is_some()
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
            .ok_or_else(|| anyhow::anyhow!("Language frontend not available for {language:?}"))?;
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
        match self
            .lazy_service
            .ensure_for_position(file_id, line, column, None)
        {
            Ok(window) => {
                lazy_summary = Some(LazySummary {
                    triggered: true,
                    units_built: window.units_built,
                    units_cached: window.units_cached,
                    units_pending: window.units_pending,
                    pending_job_ids: window.pending_job_ids.clone(),
                    truncated: window.truncated,
                    duration_ms: lazy_start.elapsed().as_millis() as u64,
                    precision_tier: window.precision_tier,
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
        resp.lazy_summary = lazy_summary;
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

    /// Trace forward from a source symbol to a target symbol.
    ///
    /// Answers "how does A reach B?" by walking forward through call edges.
    /// Returns a [`types::caller_path::ForwardChain`] if a path exists.
    pub fn trace_forward(
        &self,
        source_id: &SymbolId,
        target_id: &SymbolId,
        max_depth: usize,
    ) -> analysis::trace::TraceQueryResponse<types::caller_path::ForwardChain> {
        self.trace.trace_forward(source_id, target_id, max_depth)
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
